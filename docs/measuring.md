# Measuring the round trip

The patch changes an encoder configuration. Whether that reaches the person
listening is a separate question, and the only honest way to answer it is to
send a known signal through a real call and look at what comes back.

`tools/roundtrip.py` does the measuring. It needs `numpy`, plus `matplotlib`
for the chart, and PipeWire's `pw-play` / `pw-record`.

## What it measures

| Metric | What it tells you |
| --- | --- |
| round trip | milliseconds from playing the probe to hearing it at the far end |
| L/R correlation | ~0 means the channels stayed independent (stereo); ~1 means they are the same signal (mono, or dual-mono) |
| bandwidth | where the path stops passing audio — a 32 kHz or SILK/hybrid stream cuts off far below a 48 kHz CELT one |
| <100 Hz vs band | how much the path attenuates the bottom end, which is what the filter bypass changes |

The probe is an 11-second stereo signal: a short identical chirp for the delay
measurement, then eight seconds where the left channel sweeps 20 Hz → 20 kHz
while the right sweeps 20 kHz → 20 Hz. Because the two channels carry different
content, anything that folds the stream to mono makes the recorded channels
identical. That is a much stronger test than looking for a level difference —
dual-mono and true stereo both have two channels, and only correlation tells
them apart.

Frequency response is computed as recorded-spectrum minus probe-spectrum. A log
sweep deliberately puts more energy per Hz at the bottom of its range, so the
raw recorded spectrum would mostly describe the probe rather than the path.

## You need a second endpoint, and it has to be a desktop client

A client does not decode its own transmission, so the encoder cannot be
measured from inside one Discord client. You need a second endpoint in the
voice channel, on a **different account**: joining from a second device on the
same account moves you rather than adding you.

Two constraints make this harder than it looks.

**Discord in a browser will not do stereo.** It is reported not to negotiate a
stereo stream, so a measurement taken against a browser comes back mono no
matter what the sending client does. That is a false negative: the patch looks
broken when the receiver is the limitation. Use a Discord **desktop** client as
the receiver.

**Discord only runs one desktop instance per machine.** `--user-data-dir` does
not help — Discord ignores it and re-uses the default profile, and a second
launch prints `Quitting secondary instance` and exits. So the two endpoints
cannot both be desktop clients on one machine.

That leaves:

- **Someone else on Discord desktop** records their end and sends you the WAV.
  They need the capture half of this setup, nothing else: `setup`, route their
  Discord output to `stereocord_capture`, and record while you play the probe.
  This is the least effort and adds no variables to the sending side.
- **A second machine or a VM** with its own Discord install.
- **A phone** on a second account, if you can capture its output digitally.
  Going out its headphone jack and back in through an interface adds an analog
  stage, and a mono input will destroy the very thing being measured.

If none of those is available, the round trip cannot be measured. The patch can
still be verified the cheap way: ask someone on desktop whether you sound
stereo.

## Procedure

Do the "before" run first, on an unpatched client (`stereocord restore`), then
patch and repeat. Nothing else about the setup may change between the two runs,
or you are measuring the setup.

```bash
python3 tools/roundtrip.py signal -o probe.wav
python3 tools/roundtrip.py setup
```

`setup` creates two virtual sinks and one virtual microphone:

| device | kind | purpose |
| --- | --- | --- |
| `stereocord_probe` | sink | the probe is played into this |
| `stereocord_mic` | **source** | what the sending client listens to |
| `stereocord_capture` | sink | the receiving endpoint's audio is routed here |

`stereocord_mic` exists because a null sink's monitor, while technically a
source, is hidden by most device pickers — desktop sound settings filter
monitors out and a browser will not offer one as a microphone. Remapping the
monitor produces a first-class input that applications list like any other mic.

Check the wiring before involving a call:

```bash
python3 tools/roundtrip.py loopback
```

That sends the probe through the virtual devices and back with no call in the
middle, so everything should come out unchanged: stereo, full bandwidth, and a
delay of a few tens of milliseconds that is pure buffering. If this fails, fix
it before measuring a call — otherwise the call gets the blame for a wiring
mistake.

In the **sending** client, Voice & Video:

- Input Device → `stereocord_mic`
- Noise Suppression **off** (Krisp will mangle a sweep into something unrecognisable)
- Echo Cancellation, Automatic Gain Control, Advanced Voice Activity **off**
- Input Mode → **Push to Talk**, and hold it for the whole 11 seconds

That last one matters more than it looks. With voice activity detection Discord
gates the quiet parts of the sweep, so the low and high ends of the measured
response are missing transmission rather than codec behaviour. Push-to-talk
held down, or Input Sensitivity dragged fully left, avoids it.

The **receiving** endpoint needs no device selection at all — its audio is moved
with `pactl` rather than chosen in its settings.

Join the channel from both endpoints, then route the receiver:

```bash
python3 tools/roundtrip.py streams
pactl move-sink-input <index> stereocord_capture
```

You will stop hearing the far end. That is the point — its audio is going to
the recorder now. Check the receiving side is at full volume, and that the
sender's per-user volume slider is at 100%.

Then, holding push-to-talk:

```bash
python3 tools/roundtrip.py capture -o before.wav
```

`capture` defaults to the two sinks `setup` made, so it needs no targets. Patch,
rejoin, and repeat with `-o after.wav`. Then:

```bash
python3 tools/roundtrip.py analyze before.wav -l before -o before.json
python3 tools/roundtrip.py analyze after.wav  -l after  -o after.json
python3 tools/roundtrip.py chart --before before.json --after after.json \
    -o docs/roundtrip.png --readme README.md
```

`--readme` inserts the chart and its numbers into the README between
`<!-- roundtrip:begin -->` markers, so the figures shown are always the ones
that produced the image. Re-running updates the section in place.

Finally:

```bash
python3 tools/roundtrip.py teardown
```

## Why parec and not pw-record

Recording uses PulseAudio's tools rather than PipeWire's, deliberately. A
monitor such as `stereocord_capture.monitor` is a PulseAudio name with no
PipeWire node behind it — `pw-cli info stereocord_capture.monitor` reports
"unknown global" — so `pw-record --target` cannot resolve it and falls back to
the default source without saying so. Targeting the sink node instead records
silence, because a monitor is a set of ports on that node rather than a source
in its own right. `parec` resolves monitor names correctly.

Both tools fall back silently on an unknown device, so `capture` checks the
names against what PulseAudio actually offers before it starts, and warns when
the default source is the probe microphone — that is the case where a fallback
produces a file that looks like a flawless round trip.

## Sanity checks before you trust a run

- Play `before.wav` back. You should hear the chirp and both sweeps. If it is
  silent or choppy, transmission was gated — check push-to-talk.
- `roundtrip_ms` should be clearly above the baseline `loopback` reported. The
  virtual devices alone cost tens of milliseconds depending on the graph
  quantum, and a real call adds network and jitter buffer on top. A figure at
  or below the loopback baseline means the recording picked up local playback
  rather than the far end, and the run is measuring your own soundcard.
- Both runs should report `channels: 2`. If the recording is mono the capture
  target was wrong.

## Reading the result

A working stereo patch moves the L/R correlation from near 1 to near 0. That is
the headline number; everything else is quality. Bandwidth should rise if the
client was previously negotiating a lower internal rate, and the sub-100 Hz
figure should move toward 0 dB if the filter injections applied — on builds
where they could not be applied (see the coverage table in the README) it will
not move, and that is expected rather than a failure.

If correlation stays near 1 after patching, the most common cause is a mono
input source: a mono microphone gives you two identical channels no matter what
the encoder does. Check that the probe itself is reaching Discord as stereo
before concluding the patch failed.

## Self-test

```bash
python3 tools/roundtrip.py selftest
```

Builds two synthetic recordings with known properties — a mono, band-limited,
high-passed path versus a stereo, full-band, unfiltered one — runs the analysis
over them, and checks the recovered numbers against what was injected. It exits
non-zero if any check fails, so it is a test rather than a demonstration.

```
PASS  before delay recovered            92.0 ms, injected 92
PASS  after delay recovered             88.0 ms, injected 88
PASS  mono path reads as mono           correlation 1.0000
PASS  stereo path reads as stereo       correlation -0.0038
PASS  band-limited path reads narrower  11309 Hz vs 20000 Hz
PASS  high-passed path loses low end    -6.7 dB
PASS  unfiltered path keeps low end     +0.0 dB
```

It also regenerates the chart below:

![self-test](roundtrip-selftest.png)

This chart is a test of the measurement code. It is **not** a Discord
measurement, and no numbers in this repository are presented as one.
