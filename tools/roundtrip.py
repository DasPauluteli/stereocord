#!/usr/bin/env python3
# ----------------------------------------------------------------------------
# "THE BIERWARE LICENSE" (Revision 1):
# <67437654+DasPauluteli@users.noreply.github.com> wrote this file. As long as
# you retain this notice you can do whatever you want with this stuff. If we
# meet some day you have to buy me a beer in return - Paul Neri
# ----------------------------------------------------------------------------

"""Measure what Discord's voice path actually does to audio.

The patch changes an encoder configuration. Whether that reaches the far end is
a separate question, and the only way to answer it is to send a known signal
through a real call and look at what comes back. This script generates that
signal, helps route it, and turns a recording into numbers and a chart.

It measures a *round trip*: test signal -> Discord input -> encode -> Discord's
servers -> decode -> a second endpoint's output -> recording. That needs a
second endpoint (a phone, or a browser signed in as another account) in a voice
channel with the patched client. There is no way to measure the encoder from
inside one client, because a client does not decode its own transmission.

Subcommands:
  signal    write the test signal to a WAV
  selftest  validate the analysis against signals with known properties
  setup     create the virtual sinks a measurement needs
  loopback  check the routing carries the probe, before involving a call
  streams   list playback streams, to route the receiving endpoint
  teardown  remove the virtual sinks
  devices   list PipeWire nodes, to find capture/playback targets
  capture   play the signal and record the far end simultaneously
  analyze   turn one recording into measurements (JSON)
  chart     render before/after measurements as a PNG

Requires numpy; chart additionally requires matplotlib.
"""

import argparse
import json
import math
import subprocess
import sys
import wave

import numpy as np

SR = 48000
SYNC_START, SYNC_LEN = 0.5, 0.5
SWEEP_START, SWEEP_LEN = 2.0, 8.0
TOTAL = 11.0


# --------------------------------------------------------------------------
# test signal
# --------------------------------------------------------------------------

def log_sweep(n, f0, f1, sr=SR):
    """Exponential sine sweep from f0 to f1 over n samples."""
    t = np.arange(n) / sr
    T = n / sr
    k = math.log(f1 / f0)
    return np.sin(2 * np.pi * f0 * T / k * (np.exp(t * k / T) - 1.0))


def build_signal():
    """Stereo probe.

    The two channels carry *different* sweeps — left rising, right falling — so
    the channels are close to uncorrelated. Anything that folds the stream to
    mono makes the two recorded channels identical, which is a far more robust
    stereo test than looking for a level difference: dual-mono and true stereo
    both have two channels, and only correlation tells them apart.

    A short identical chirp near the start gives cross-correlation something
    unambiguous to lock onto for the delay measurement.
    """
    n = int(TOTAL * SR)
    left = np.zeros(n)
    right = np.zeros(n)

    s0, sn = int(SYNC_START * SR), int(SYNC_LEN * SR)
    sync = log_sweep(sn, 500, 8000) * np.hanning(sn)
    left[s0:s0 + sn] = sync
    right[s0:s0 + sn] = sync

    w0, wn = int(SWEEP_START * SR), int(SWEEP_LEN * SR)
    fade = np.ones(wn)
    edge = int(0.05 * SR)
    fade[:edge] = np.linspace(0, 1, edge)
    fade[-edge:] = np.linspace(1, 0, edge)
    left[w0:w0 + wn] = log_sweep(wn, 20, 20000) * fade
    right[w0:w0 + wn] = log_sweep(wn, 20000, 20) * fade

    # Leave headroom: Discord applies its own gain, and a clipped probe would
    # produce harmonics that look like codec artefacts.
    return np.stack([left, right], axis=1) * 0.5


def write_wav(path, data, sr=SR):
    pcm = np.clip(data, -1.0, 1.0)
    pcm = (pcm * 32767.0).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(pcm.shape[1] if pcm.ndim > 1 else 1)
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm.tobytes())


def read_wav(path):
    with wave.open(path, "rb") as w:
        ch, sw, sr = w.getnchannels(), w.getsampwidth(), w.getframerate()
        raw = w.readframes(w.getnframes())
    if sw != 2:
        raise SystemExit(f"{path}: expected 16-bit PCM, got {sw * 8}-bit")
    data = np.frombuffer(raw, dtype="<i2").astype(np.float64) / 32768.0
    if ch > 1:
        data = data.reshape(-1, ch)
    else:
        data = data.reshape(-1, 1)
    return data, sr


# --------------------------------------------------------------------------
# measurements
# --------------------------------------------------------------------------

def find_delay(rec, ref, sr):
    """Round-trip delay in ms, from cross-correlation of the sync chirp.

    Correlating against only the chirp (rather than the whole probe) keeps the
    peak sharp: the sweep is self-similar over long lags and would smear it.
    """
    s0, sn = int(SYNC_START * sr), int(SYNC_LEN * sr)
    template = ref[s0:s0 + sn, 0]
    probe = rec[:, 0]
    if len(probe) < len(template):
        return None
    n = 1 << (len(probe) + len(template) - 1).bit_length()
    corr = np.fft.irfft(np.fft.rfft(probe, n) * np.conj(np.fft.rfft(template, n)), n)
    lag = int(np.argmax(np.abs(corr[: len(probe)])))
    return (lag - s0) / sr * 1000.0


def channel_correlation(seg):
    """Pearson correlation of the two channels.

    ~0 means the channels stayed independent (true stereo). ~1 means they are
    the same signal — either a mono downmix or dual-mono.
    """
    if seg.shape[1] < 2:
        return 1.0
    left, right = seg[:, 0], seg[:, 1]
    if np.std(left) < 1e-9 or np.std(right) < 1e-9:
        return 1.0
    return float(np.corrcoef(left, right)[0, 1])


def spectrum(x, sr, nfft=8192):
    """Averaged magnitude spectrum in dB, via Welch-style segment averaging.

    Returned un-normalised so two spectra can be divided to get a response.
    """
    if len(x) < nfft:
        return np.array([]), np.array([])
    win = np.hanning(nfft)
    step = nfft // 2
    acc = np.zeros(nfft // 2 + 1)
    count = 0
    for i in range(0, len(x) - nfft, step):
        acc += np.abs(np.fft.rfft(x[i:i + nfft] * win)) ** 2
        count += 1
    if count == 0:
        return np.array([]), np.array([])
    freqs = np.fft.rfftfreq(nfft, 1 / sr)
    return freqs, 10 * np.log10(acc / count + 1e-20)


def response(rec, ref, sr):
    """Frequency response of the path: recorded spectrum minus the probe's own.

    Measuring the raw recorded spectrum would mostly describe the probe. A log
    sweep deliberately puts more energy per Hz at the bottom of the range, which
    on its own looks like a bass boost — dividing it out leaves only what the
    path actually did. Normalised so 200-1000 Hz sits at 0 dB.
    """
    f, rec_db = spectrum(rec, sr)
    _, ref_db = spectrum(ref, sr)
    if len(f) == 0 or len(ref_db) == 0:
        return np.array([]), np.array([])
    resp = rec_db - ref_db

    # Only trust bins where the probe actually put energy. The sweep tapers off
    # at its endpoints, and there the ratio is one noise floor over another,
    # which produces enormous meaningless values. Those bins become NaN.
    ref_band = ref_db[(f >= 200) & (f <= 1000)]
    if len(ref_band):
        floor = float(np.median(ref_band)) - 35.0
        resp = np.where(ref_db >= floor, resp, np.nan)

    band = resp[(f >= 200) & (f <= 1000)]
    band = band[~np.isnan(band)]
    if len(band):
        resp = resp - float(np.median(band))
    return f, resp


# The probe sweeps 20 Hz to 20 kHz, so above 20 kHz there is no reference
# energy and the response is a ratio of two noise floors. Never read bandwidth
# from up there.
PROBE_TOP_HZ = 20000.0


def smooth(resp, width=9):
    """Moving median, to keep a single noisy bin from deciding the answer."""
    if len(resp) < width:
        return resp
    pad = width // 2
    padded = np.concatenate([resp[:pad][::-1], resp, resp[-pad:][::-1]])
    return np.array([np.nanmedian(padded[i:i + width]) for i in range(len(resp))])


def rolloff(freqs, resp, drop=-20.0, floor_hz=1000.0):
    """Where the path stops passing audio, in Hz.

    Reads out the codec's effective bandwidth: an Opus stream running at 32 kHz
    internally, or one that fell back to a SILK/hybrid mode, cuts off well below
    a 48 kHz CELT stream.

    Scans upward and returns the first sustained drop below `drop`, rather than
    the last bin that happens to poke above it — a codec's cutoff is a cliff,
    and one stray bin above the cliff should not move the answer.
    """
    if len(freqs) == 0:
        return None
    band = (freqs >= floor_hz) & (freqs <= PROBE_TOP_HZ) & ~np.isnan(resp)
    f, r = freqs[band], smooth(resp[band])
    if len(f) == 0:
        return None
    # "Sustained" = stays below the threshold for the rest of the probe band,
    # allowing a little slack for noise.
    for i in range(len(f)):
        if r[i] <= drop and np.mean(r[i:] <= drop) > 0.9:
            return float(f[i])
    return float(PROBE_TOP_HZ)


def low_energy(freqs, resp, hz=100.0):
    """How much the path attenuates below `hz`, relative to the passband.

    The high-pass and DC-rejection filters this project bypasses are what remove
    energy down here, so a stock client reads clearly negative and a fully
    patched one reads near 0.
    """
    if len(freqs) == 0:
        return None
    low = resp[(freqs > 20) & (freqs < hz)]
    low = low[~np.isnan(low)]
    return float(np.median(low)) if len(low) else None


def codec_residual_db(seg, ref_seg):
    """How far the recording departs from the probe, in dB.

    A lossy codec cannot reproduce a full-band sweep exactly. A digital
    loopback can, and does, to around -90 dB. This is therefore the most
    reliable way to tell "went through Discord" from "never left the machine",
    and it does not care *why* the routing was wrong.

    Channel folding also shows up here, since a mono downmix makes the left
    channel differ from the left of the probe. Either way, a near-zero residual
    means no codec was involved.
    """
    n = min(len(seg), len(ref_seg))
    if n < 1000:
        return None
    a, b = seg[:n, 0], ref_seg[:n, 0]
    denom = float(np.dot(b, b))
    if denom < 1e-12:
        return None
    scale = float(np.dot(a, b)) / denom
    resid = a - scale * b
    rms_a = float(np.sqrt((a ** 2).mean()))
    if rms_a < 1e-9:
        return None
    return float(20 * np.log10(np.sqrt((resid ** 2).mean()) / rms_a + 1e-20))


# Opus never gets near this on a full-band sweep; a digital loopback sits
# around -90 dB. Anything below means nothing encoded the signal.
NO_CODEC_DB = -60.0


def analyze(rec_path, label):
    rec, sr = read_wav(rec_path)
    ref = build_signal()

    delay = find_delay(rec, ref, sr)
    # Align the analysis window to where the sweep actually landed.
    shift = int((delay or 0.0) / 1000.0 * sr)
    w0 = int(SWEEP_START * sr) + shift
    wn = int(SWEEP_LEN * sr)
    w0 = max(0, min(w0, max(0, len(rec) - 1)))
    seg = rec[w0:w0 + wn]
    if len(seg) < sr:
        raise SystemExit(
            f"{rec_path}: only {len(seg)/sr:.1f}s of sweep found after alignment; "
            "the recording is probably too short or missed the signal"
        )

    corr = channel_correlation(seg)
    w0r, wnr = int(SWEEP_START * sr), int(SWEEP_LEN * sr)
    ref_seg = ref[w0r:w0r + wnr]
    n = min(len(seg), len(ref_seg))
    f_l, db_l = response(seg[:n, 0], ref_seg[:n, 0], sr)
    if seg.shape[1] > 1:
        f_r, db_r = response(seg[:n, 1], ref_seg[:n, 1], sr)
    else:
        f_r, db_r = f_l, db_l

    residual = codec_residual_db(seg[:n], ref_seg[:n])

    return {
        "label": label,
        "source": rec_path,
        "codec_residual_db": residual,
        "went_through_codec": None if residual is None else residual > NO_CODEC_DB,
        "sample_rate": sr,
        "channels": int(seg.shape[1]),
        "roundtrip_ms": delay,
        "channel_correlation": corr,
        "stereo": bool(abs(corr) < 0.5),
        "bandwidth_hz": rolloff(f_l, db_l),
        "low_freq_db": low_energy(f_l, db_l),
        "response": {
            "freqs": f_l[::8].tolist(),
            "left": [None if np.isnan(v) else v for v in db_l[::8]],
            "right": [None if np.isnan(v) else v for v in db_r[::8]],
        },
    }


# --------------------------------------------------------------------------
# capture
# --------------------------------------------------------------------------

PROBE_SINK = "stereocord_probe"
CAPTURE_SINK = "stereocord_capture"
PROBE_SOURCE = "stereocord_mic"


def _pactl(*args, check=True):
    return subprocess.run(["pactl", *args], capture_output=True, text=True,
                          check=check).stdout


def _module_ids():
    """Module ids of everything this script loaded."""
    return [line.split()[0]
            for line in _pactl("list", "short", "modules", check=False).splitlines()
            if "stereocord" in line]


def setup():
    """Create the two virtual sinks a measurement needs.

    One is what the sending client listens to, so the probe reaches Discord
    without going near a real microphone. The other is where the receiving
    endpoint's audio is sent, so the recording contains only the far end and
    not whatever else the machine happens to be playing.
    """
    existing = _pactl("list", "short", "sinks", check=False)
    for name in (PROBE_SINK, CAPTURE_SINK):
        if name in existing:
            print(f"  {name} already exists")
            continue
        _pactl("load-module", "module-null-sink", f"sink_name={name}",
               f"sink_properties=device.description={name}")
        print(f"  created sink {name}")

    # A null sink's monitor is a source, but most device pickers hide monitors:
    # desktop sound settings filter them out, and a browser will not offer one
    # as a microphone. Remapping the monitor produces a first-class input that
    # applications list like any other microphone.
    if PROBE_SOURCE not in _pactl("list", "short", "sources", check=False):
        _pactl("load-module", "module-remap-source",
               f"source_name={PROBE_SOURCE}", f"master={PROBE_SINK}.monitor",
               f"source_properties=device.description={PROBE_SOURCE}")
        print(f"  created source {PROBE_SOURCE}")
    else:
        print(f"  {PROBE_SOURCE} already exists")

    print(f"""
Check the routing before involving a call:

  python3 tools/roundtrip.py loopback

Now, in the SENDING Discord (the patched desktop client):
  Voice & Video -> Input Device -> "{PROBE_SOURCE}"
  Turn OFF noise suppression, echo cancellation and automatic gain control.
  Set Input Mode to Push to Talk and hold it during the capture, or drag
  Input Sensitivity fully left so it always transmits. Voice activity
  detection will gate the quiet parts of the sweep and ruin the measurement.

Then join a voice channel from a SECOND endpoint signed in as a different
account. It needs no device selection of its own - its audio is moved with
pactl rather than chosen in its settings:

  python3 tools/roundtrip.py streams        # find its index
  pactl move-sink-input <index> {CAPTURE_SINK}

You will stop hearing the far end - that is expected, its audio is now going
to the recorder instead of your speakers.""")


def teardown():
    ids = _module_ids()
    if not ids:
        print("  nothing to remove")
        return
    for mid in ids:
        _pactl("unload-module", mid, check=False)
        print(f"  unloaded module {mid}")


def list_streams():
    """Playback streams, so the receiving endpoint can be routed.

    Shows which sink each one is on and what it is playing, because
    "Chromium" alone does not distinguish a browser from Discord's own
    Electron process, and routing the wrong one is silent and plausible.
    """
    sinks = {}
    for line in _pactl("list", "short", "sinks", check=False).splitlines():
        f = line.split("\t")
        if len(f) > 1:
            sinks[f[0]] = f[1]

    rows, cur = [], None
    for line in _pactl("list", "sink-inputs", check=False).splitlines():
        s = line.strip()
        if s.startswith("Sink Input #"):
            if cur:
                rows.append(cur)
            cur = {"index": s.split("#")[1], "app": "?", "media": "", "sink": "?"}
        elif cur is not None:
            if s.startswith("Sink:"):
                cur["sink"] = sinks.get(s.split(":", 1)[1].strip(), "?")
            elif s.startswith("application.name ="):
                cur["app"] = s.split("=", 1)[1].strip().strip('"')
            elif s.startswith("media.name ="):
                cur["media"] = s.split("=", 1)[1].strip().strip('"')[:34]
    if cur:
        rows.append(cur)

    if not rows:
        print("  no playback streams. The far end has to actually be making sound\n"
              "  before it appears here - have it play something, or unmute briefly.")
        return

    print(f"  {'index':>5}  {'application':<22} {'playing':<34} currently on")
    for r in rows:
        mark = " <- already routed" if r["sink"] == CAPTURE_SINK else ""
        print(f"  {r['index']:>5}  {r['app']:<22} {r['media']:<34} {r['sink']}{mark}")
    routed = [r for r in rows if r["sink"] == CAPTURE_SINK]
    print(f"""
  Route the RECEIVING endpoint, not the sending Discord. "WEBRTC VoiceEngine"
  is Discord's own desktop client - moving that one records the sender's
  playback rather than the far end.

  pactl move-sink-input <index> {CAPTURE_SINK}""")

    if len(routed) > 1:
        apps = ", ".join(sorted({r["app"] for r in routed}))
        print(f"""
  {len(routed)} streams are on {CAPTURE_SINK} ({apps}). That is usually fine:
  a browser keeps a playback stream open per renderer even in silence, and all
  its tabs share one audio process, so they cannot be told apart here. Whichever
  one carries the call is already routed.

  It only matters if something else plays during the capture, since that lands
  in the recording too. Check the tap is quiet before recording:

    parec -d {CAPTURE_SINK}.monitor --file-format=wav /tmp/q.wav & sleep 3; kill %1

  With the far end silent, that file should be silence.""")


def loopback(tmp_wav):
    """Send the probe through the virtual devices and measure the result.

    No call involved, so nothing should change: this checks the routing, not
    Discord. A clean result here means a later bad measurement is Discord's
    doing rather than a wiring mistake, which is worth knowing before spending
    the effort of setting up two accounts.
    """
    import os

    if not os.path.exists("probe.wav"):
        write_wav("probe.wav", build_signal())
        print("  wrote probe.wav")
    capture("probe.wav", tmp_wav, PROBE_SINK, PROBE_SOURCE, TOTAL + 3)
    r = analyze(tmp_wav, "loopback")
    checks = [
        ("recording is stereo", r["channels"] == 2, f"{r['channels']} channels"),
        ("channels stayed independent", r["stereo"],
         f"correlation {r['channel_correlation']:.4f}"),
        ("full bandwidth", (r["bandwidth_hz"] or 0) >= 18000,
         f"{r['bandwidth_hz']:.0f} Hz"),
        # Not a tightness check. Local buffering depends on the graph quantum
        # and how many nodes the signal hops through, and legitimately ranges
        # from a few milliseconds to a couple of hundred. What matters is that
        # a delay was recoverable at all: a wild or negative value means
        # cross-correlation found no peak, i.e. the recording holds no probe.
        ("delay is recoverable", 0 <= (r["roundtrip_ms"] or -1) < 500,
         f"{r['roundtrip_ms']:.1f} ms"),
    ]
    width = max(len(n) for n, _, _ in checks)
    failed = 0
    for name, ok, detail in checks:
        print(f"  {'PASS' if ok else 'FAIL'}  {name:<{width}}  {detail}")
        failed += not ok
    if failed:
        print("\n  The virtual devices are not carrying the probe intact. Fix this\n"
              "  before measuring a call, or the call will get the blame.")
    else:
        print(f"""
  Routing is good. The probe survives the virtual devices unchanged, so
  anything a real run shows is Discord's doing.

  Baseline latency of the routing itself: {r['roundtrip_ms']:.0f} ms. A real call should
  come back clearly above that; a result at or below it means the recording
  picked up local playback rather than the far end.""")
    return 1 if failed else 0


def list_devices():
    try:
        out = subprocess.run(
            ["pactl", "list", "short", "sources"], capture_output=True, text=True, check=True
        ).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        raise SystemExit(f"cannot list audio sources: {e}")
    print("Recording sources (use one as --record-target):\n")
    print(out.rstrip())
    print(
        "\nA '.monitor' source captures what an application is playing. To record\n"
        "the far end of the call, pick the monitor of whatever is playing it."
    )


def _device_names(kind):
    """Exact PulseAudio device names, which is what parec/paplay resolve."""
    return {line.split("\t")[1]
            for line in _pactl("list", "short", kind, check=False).splitlines()
            if "\t" in line}


def capture(signal_path, out_path, play_target, record_target, seconds):
    # Both parec and paplay fall back to the default device when the one named
    # does not resolve. They do it silently and still write a plausible file,
    # which is how a measurement ends up describing the local soundcard. Match
    # the names exactly against what PulseAudio actually offers.
    for role, target, kind in (("play", play_target, "sinks"),
                               ("record", record_target, "sources")):
        if target and target not in _device_names(kind):
            raise SystemExit(
                f"{role} device {target!r} does not exist.\n"
                "Run 'roundtrip.py setup' first - and note that 'teardown' removes\n"
                "these devices, so anything recorded afterwards goes to the default\n"
                "device instead, which is not a measurement of anything.")

    # A fallback only looks convincing when the default source is carrying the
    # probe, which is exactly the case after pointing a client at the virtual
    # microphone. Say so, because the resulting file looks perfectly fine.
    default_source = _pactl("get-default-source", check=False).strip()
    if default_source == PROBE_SOURCE and record_target != PROBE_SOURCE:
        print(f"  note: the default source is {PROBE_SOURCE}, which carries the probe.\n"
              f"        If anything falls back to it the recording will look like a\n"
              f"        clean round trip while never having left this machine.")

    # PulseAudio's tools rather than PipeWire's: a monitor such as
    # "stereocord_capture.monitor" is a PulseAudio name with no PipeWire node
    # behind it, so pw-record cannot resolve it and falls back to the default
    # source. Targeting the sink node instead records silence, because a
    # monitor is a set of ports on that node rather than a source of its own.
    play = ["paplay"] + (["--device=" + play_target] if play_target else []) + [signal_path]
    rec = ["parec"] + (["--device=" + record_target] if record_target else []) + [
        "--file-format=wav", "--format=s16le",
        f"--rate={SR}", "--channels=2", out_path,
    ]
    print(f"recording -> {out_path}")
    recorder = subprocess.Popen(rec)
    try:
        subprocess.run(play, check=False)
        # Keep recording past the end of playback so the tail of the round trip,
        # which arrives late by definition, is not cut off.
        try:
            recorder.wait(timeout=max(1.0, seconds - TOTAL) + 2.0)
        except subprocess.TimeoutExpired:
            pass
    finally:
        recorder.terminate()
        try:
            recorder.wait(timeout=5)
        except subprocess.TimeoutExpired:
            recorder.kill()
    print("done")


# --------------------------------------------------------------------------
# chart
# --------------------------------------------------------------------------

def chart(before, after, out_path, title):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.ticker import FixedLocator, FuncFormatter, NullFormatter

    fig, axes = plt.subplots(1, 3, figsize=(14, 4.6),
                             gridspec_kw={"width_ratios": [1.5, 1.0, 1.1]})
    fig.suptitle(title, fontsize=13, y=0.99)

    runs = [r for r in (before, after) if r]
    # Red against blue rather than red against green: the "after" trace often
    # sits flat along the 0 dB line, so it needs to stand out from both the
    # reference line and the "before" trace — and red/green is the one pair
    # that disappears for the most common kinds of colour blindness.
    colors = {"before": "#b0413e", "after": "#0b6fd8"}

    ax = axes[0]
    for r in runs:
        c = colors["before"] if r is before else colors["after"]
        f = np.array(r["response"]["freqs"])
        # None -> NaN so matplotlib simply leaves gaps where the probe had no
        # energy, rather than drawing a line through nonsense.
        left = np.array([np.nan if v is None else v for v in r["response"]["left"]])
        right = np.array([np.nan if v is None else v for v in r["response"]["right"]])
        # Draw "after" over "before" so a flat trace is not hidden beneath it.
        z = 3 if r is after else 2
        ax.semilogx(f, left, color=c, lw=1.6, label=f"{r['label']} L", zorder=z)
        ax.semilogx(f, right, color=c, lw=1.1, ls="--", alpha=0.75,
                    label=f"{r['label']} R", zorder=z)
    ax.set_xlim(20, 24000)
    ax.set_ylim(-60, 12)
    ax.axhline(0, color="#bbb", lw=0.8, zorder=1)
    # A log axis defaults to 10^n labels. Audio bandwidths are read as plain
    # numbers, so label the decade edges and the useful points between them
    # and drop the minor labels entirely.
    ticks = [20, 50, 100, 500, 1000, 5000, 10000, 20000]
    ax.xaxis.set_major_locator(FixedLocator(ticks))
    ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{int(v)}"))
    ax.xaxis.set_minor_formatter(NullFormatter())
    ax.tick_params(axis="x", labelsize=7.5)
    ax.set_xlabel("Frequency (Hz)")
    ax.set_ylabel("dB relative to 200-1000 Hz")
    ax.set_title("Path frequency response")
    ax.grid(alpha=0.3, which="both")
    ax.legend(fontsize=7)

    ax = axes[1]
    labels = [r["label"] for r in runs]
    vals = [abs(r["channel_correlation"]) for r in runs]
    bars = ax.bar(labels, vals, color=[colors["before"] if r is before else colors["after"]
                                       for r in runs])
    ax.axhline(0.5, color="#666", ls="--", lw=1)
    ax.set_ylim(0, 1.18)
    ax.text(0.5, 0.55, "same signal in both channels above here",
            transform=ax.transAxes, ha="center", fontsize=7.5, color="#555")
    ax.set_ylabel("|L/R correlation|")
    ax.set_title("Stereo separation")
    for b, r in zip(bars, runs):
        ax.text(b.get_x() + b.get_width() / 2, b.get_height() + 0.03,
                "stereo" if r["stereo"] else "mono", ha="center", fontsize=10,
                fontweight="bold")

    ax = axes[2]
    ax.axis("off")
    rows = [("round trip", "roundtrip_ms", "{:.0f} ms"),
            ("bandwidth", "bandwidth_hz", "{:.0f} Hz"),
            ("<100 Hz vs band", "low_freq_db", "{:+.1f} dB"),
            ("channels", "channels", "{}")]
    text = [f"{'':<18}" + "".join(f"{r['label']:>12}" for r in runs)]
    for name, key, fmt in rows:
        cells = []
        for r in runs:
            v = r.get(key)
            cells.append(f"{fmt.format(v):>12}" if v is not None else f"{'n/a':>12}")
        text.append(f"{name:<18}" + "".join(cells))
    ax.text(0, 0.95, "\n".join(text), family="monospace", fontsize=10, va="top")
    ax.set_title("Measurements")

    fig.tight_layout()
    fig.savefig(out_path, dpi=140)
    print(f"wrote {out_path}")


# --------------------------------------------------------------------------
# self-test
# --------------------------------------------------------------------------

def _shape(x, lp=None, hp=None, hp_order=2, floor_db=None, sr=SR):
    """Apply a low-pass and/or high-pass in the frequency domain.

    `hp_order` sets how abruptly the high-pass falls: a stock client's low end
    does not slope away gently, it drops off a cliff. `floor_db` caps how deep
    the attenuation goes, because a real path bottoms out on the codec's own
    residual rather than continuing down forever.
    """
    n = len(x)
    X = np.fft.rfft(x)
    f = np.fft.rfftfreq(n, 1 / sr)
    H = np.ones(len(f))
    if lp:
        H *= 1 / np.sqrt(1 + (f / lp) ** 12)
    if hp:
        H *= (f / hp) ** hp_order / np.sqrt(1 + (f / hp) ** (2 * hp_order))
    if floor_db is not None:
        H = np.maximum(H, 10 ** (floor_db / 20))
    return np.fft.irfft(X * H, n)


def _simulate(delay_ms, mono, lp=None, hp=None, hp_order=2, floor_db=None,
              noise=2e-4, sr=SR):
    """A recording of the probe through a path with known properties."""
    ref = build_signal()
    d = int(delay_ms / 1000 * sr)
    out = np.zeros((len(ref) + d, 2))
    left, right = ref[:, 0], ref[:, 1]
    if mono:
        m = (left + right) / 2
        left = right = m
    kw = dict(lp=lp, hp=hp, hp_order=hp_order, floor_db=floor_db, sr=sr)
    out[d:, 0] = _shape(left, **kw)
    out[d:, 1] = _shape(right, **kw)
    return out + np.random.default_rng(7).normal(0, noise, out.shape)


# The "before" and "after" cases stand in for a stock and a patched client, and
# are shaped to resemble the round trip actually measured between two desktop
# clients (docs/roundtrip.png): both arms full band, differing in channels and
# in the low end. The stock arm's high-pass is steep and deep because the real
# one is - the measured capture reads -40.5 dB below 100 Hz with the passband
# intact by ~150 Hz, which these numbers reproduce to within 2 dB.
#
# The earlier "before" here also band-limited to 7.8 kHz, on the assumption
# that a stock client falls back to a hybrid mode. The real one did not: it ran
# full band and only the channels and the low end changed. That assumption is
# now confined to `narrowband`, which is not charted and exists only to give
# the bandwidth measurement a known cutoff to recover.
SELFTEST_CASES = {
    "before": dict(delay_ms=239, mono=True, hp=92, hp_order=12, floor_db=-51),
    "after": dict(delay_ms=234, mono=False),
    "narrowband": dict(delay_ms=239, mono=True, lp=7800),
}

# Where a 12th-order low-pass at `lp` crosses the -20 dB line `rolloff` looks
# for: |H| = 1/sqrt(1 + (f/lp)^12) = 0.1 at f = lp * 99^(1/12).
NARROWBAND_CUTOFF_HZ = SELFTEST_CASES["narrowband"]["lp"] * 99 ** (1 / 12)


def selftest(out_dir, keep):
    """Check the analysis against recordings whose properties are known.

    This validates the measurement code. It says nothing about Discord, and
    the chart it produces is not a Discord measurement - the two charted cases
    are shaped like the capture in docs/roundtrip.png so the comparison is not
    misleading, but the numbers in them were put there by hand.
    """
    import os

    os.makedirs(out_dir, exist_ok=True)
    results = {}
    for label, spec in SELFTEST_CASES.items():
        wav = os.path.join(out_dir, f"selftest_{label}.wav")
        write_wav(wav, _simulate(**spec))
        results[label] = analyze(wav, label)
        if not keep:
            os.remove(wav)

    b, a, nb = results["before"], results["after"], results["narrowband"]
    b_ms = SELFTEST_CASES["before"]["delay_ms"]
    a_ms = SELFTEST_CASES["after"]["delay_ms"]
    cut = NARROWBAND_CUTOFF_HZ
    checks = [
        ("before delay recovered",
         abs(b["roundtrip_ms"] - b_ms) < 2,
         f"{b['roundtrip_ms']:.1f} ms, injected {b_ms}"),
        ("after delay recovered",
         abs(a["roundtrip_ms"] - a_ms) < 2,
         f"{a['roundtrip_ms']:.1f} ms, injected {a_ms}"),
        ("mono path reads as mono",
         b["channel_correlation"] > 0.9 and not b["stereo"],
         f"correlation {b['channel_correlation']:.4f}"),
        ("stereo path reads as stereo",
         abs(a["channel_correlation"]) < 0.1 and a["stereo"],
         f"correlation {a['channel_correlation']:.4f}"),
        ("full-band paths read full band",
         b["bandwidth_hz"] == PROBE_TOP_HZ and a["bandwidth_hz"] == PROBE_TOP_HZ,
         f"{b['bandwidth_hz']:.0f} Hz and {a['bandwidth_hz']:.0f} Hz"),
        ("band-limited path reads its cutoff",
         abs(nb["bandwidth_hz"] - cut) / cut < 0.05,
         f"{nb['bandwidth_hz']:.0f} Hz, injected {cut:.0f}"),
        ("high-passed path loses low end",
         b["low_freq_db"] < -30.0, f"{b['low_freq_db']:+.1f} dB"),
        ("unfiltered path keeps low end",
         abs(a["low_freq_db"]) < 1.0, f"{a['low_freq_db']:+.1f} dB"),
    ]

    width = max(len(n) for n, _, _ in checks)
    failed = 0
    for name, ok, detail in checks:
        print(f"  {'PASS' if ok else 'FAIL'}  {name:<{width}}  {detail}")
        failed += not ok

    for label, r in results.items():
        with open(os.path.join(out_dir, f"selftest_{label}.json"), "w") as f:
            json.dump(r, f, indent=2)

    png = os.path.join(out_dir, "roundtrip-selftest.png")
    try:
        chart(b, a, png,
              "What the patch changes - simulated signal path, not a Discord capture")
    except ImportError:
        print("  (matplotlib missing, chart skipped)")

    print(f"\n  {len(checks) - failed}/{len(checks)} checks passed")
    return 1 if failed else 0


BEGIN = "<!-- roundtrip:begin -->"
END = "<!-- roundtrip:end -->"


def install_readme(readme, png, before, after):
    """Drop the chart and its numbers into the README, between markers.

    Done from the measurement rather than by hand so the figures under the
    image are always the ones that produced it, and so the section only exists
    once a real measurement has been taken - a README referencing a chart that
    was never generated is worse than no section at all.
    """
    import os

    rel = os.path.relpath(png, os.path.dirname(os.path.abspath(readme)) or ".")

    def cell(r, key, fmt):
        v = r.get(key) if r else None
        return fmt.format(v) if v is not None else "n/a"

    rows = [
        ("channels are", "channel_correlation",
         lambda r: "**stereo**" if r["stereo"] else "mono"),
        ("L/R correlation", "channel_correlation", lambda r: f"{r['channel_correlation']:.3f}"),
        ("bandwidth", "bandwidth_hz", lambda r: cell(r, "bandwidth_hz", "{:.0f} Hz")),
        ("round trip", "roundtrip_ms", lambda r: cell(r, "roundtrip_ms", "{:.0f} ms")),
        ("below 100 Hz", "low_freq_db", lambda r: cell(r, "low_freq_db", "{:+.1f} dB")),
    ]
    table = ["| | before | after |", "| --- | --- | --- |"]
    for label, _, fn in rows:
        table.append(f"| {label} | {fn(before)} | {fn(after)} |")

    section = f"""{BEGIN}
## Before & after

A real round trip through a Discord call, between two Discord desktop clients
on separate machines: the probe goes into the sending client, out through
Discord's servers, and is recorded at the receiving end. Only the sender is
patched — the receiving client is a stock, unmodified install. Measured with
[`tools/roundtrip.py`](tools/roundtrip.py); see
[docs/measuring.md](docs/measuring.md) for the procedure.

![before and after]({rel})

{chr(10).join(table)}

The headline number is the L/R correlation. Two channels carrying the same
signal are mono however many channels the container claims.
{END}"""

    text = open(readme).read()
    if BEGIN in text and END in text:
        head, rest = text.split(BEGIN, 1)
        text = head + section + rest.split(END, 1)[1]
    else:
        anchor = "## Documentation"
        if anchor in text:
            text = text.replace(anchor, section + "\n\n" + anchor, 1)
        else:
            text = text.rstrip() + "\n\n" + section + "\n"
    open(readme, "w").write(text)
    print(f"updated {readme}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("signal", help="write the test signal")
    p.add_argument("-o", "--out", default="probe.wav")

    sub.add_parser("devices", help="list PipeWire sources")
    sub.add_parser("setup", help="create the probe and capture sinks")
    sub.add_parser("teardown", help="remove them again")
    sub.add_parser("streams", help="list playback streams, to route the far end")
    p = sub.add_parser("loopback", help="check the routing without involving a call")
    p.add_argument("-o", "--out", default="loopback.wav")

    p = sub.add_parser("capture", help="play the probe and record the far end")
    p.add_argument("-s", "--signal", default="probe.wav")
    p.add_argument("-o", "--out", required=True)
    p.add_argument("--play-target", default=PROBE_SINK,
                   help="node to play into (what the sending client listens to)")
    p.add_argument("--record-target", default=CAPTURE_SINK + ".monitor",
                   help="node to record from (where the far end's audio goes)")
    p.add_argument("--seconds", type=float, default=TOTAL + 3)

    p = sub.add_parser("analyze", help="measure one recording")
    p.add_argument("recording")
    p.add_argument("-l", "--label", default="run")
    p.add_argument("-o", "--out", help="write JSON here")

    p = sub.add_parser("selftest", help="validate the analysis against known signals")
    p.add_argument("-o", "--out-dir", default="docs")
    p.add_argument("--keep-wavs", action="store_true",
                   help="leave the synthetic recordings on disk")

    p = sub.add_parser("chart", help="render before/after")
    p.add_argument("--before")
    p.add_argument("--after")
    p.add_argument("-o", "--out", default="roundtrip.png")
    p.add_argument("-t", "--title", default="Discord voice round trip")
    p.add_argument("--readme", metavar="PATH",
                   help="also insert the chart and its numbers into this README")

    a = ap.parse_args()

    if a.cmd == "signal":
        write_wav(a.out, build_signal())
        print(f"wrote {a.out} ({TOTAL:.0f}s, {SR} Hz stereo)")
    elif a.cmd == "devices":
        list_devices()
    elif a.cmd == "setup":
        setup()
    elif a.cmd == "teardown":
        teardown()
    elif a.cmd == "streams":
        list_streams()
    elif a.cmd == "loopback":
        return loopback(a.out)
    elif a.cmd == "capture":
        capture(a.signal, a.out, a.play_target, a.record_target, a.seconds)
    elif a.cmd == "analyze":
        r = analyze(a.recording, a.label)
        blob = json.dumps(r, indent=2)
        if a.out:
            with open(a.out, "w") as f:
                f.write(blob)
            print(f"wrote {a.out}")
        summary = {k: v for k, v in r.items() if k != "response"}
        print(json.dumps(summary, indent=2))
        if r["went_through_codec"] is False:
            print(f"""
WARNING: this recording never went through a codec.

  The probe came back with a residual of {r['codec_residual_db']:.0f} dB, which
  means it is a bit-exact copy. Opus cannot do that. The audio looped back
  through the virtual devices without passing through Discord, so every number
  above describes your own soundcard rather than the call.

  Most likely the record target did not resolve and pw-record fell back to the
  default source. Check that the receiving endpoint's audio really is going to
  {CAPTURE_SINK}, and that the sinks still exist (teardown removes them).""")
            return 1
    elif a.cmd == "selftest":
        return selftest(a.out_dir, a.keep_wavs)
    elif a.cmd == "chart":
        def load(p):
            if not p:
                return None
            with open(p) as f:
                return json.load(f)
        before, after = load(a.before), load(a.after)
        if not before and not after:
            raise SystemExit("need --before and/or --after")
        bad = [r["label"] for r in (before, after)
               if r and r.get("went_through_codec") is False]
        if bad:
            raise SystemExit(
                f"refusing to chart: {', '.join(bad)} never went through a codec, so "
                "the numbers describe local playback rather than a call. Re-record.")
        chart(before, after, a.out, a.title)
        if a.readme:
            if not (before and after):
                raise SystemExit("--readme needs both --before and --after")
            install_readme(a.readme, a.out, before, after)


if __name__ == "__main__":
    sys.exit(main())
