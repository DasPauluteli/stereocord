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

## You need a second endpoint

A client does not decode its own transmission, so there is no way to measure the
encoder from inside one Discord client. You need a second endpoint — a phone, or
a browser signed in as another account — in a voice channel with the patched
client. The measurement is of the whole path: your input → encode → Discord's
servers → decode → the second endpoint's output.

## Procedure

Do the "before" run first, on an unpatched client (`stereocord restore` if
needed), then patch and repeat. Nothing else about the setup may change between
the two runs, or you are measuring the setup.

```bash
python3 tools/roundtrip.py signal -o probe.wav
```

Create a null sink and point Discord's input at its monitor:

```bash
pactl load-module module-null-sink sink_name=stereocord_probe
```

In Discord: Voice & Video → Input Device → `Monitor of stereocord_probe`. Turn
off noise suppression, echo cancellation and automatic gain control — they are
input processing and will show up in the measurement as if they were the codec.

Find the source to record from — the monitor of whatever is playing the far end:

```bash
python3 tools/roundtrip.py devices
```

Join a voice channel with the second endpoint, then:

```bash
python3 tools/roundtrip.py capture -o before.wav --play-target stereocord_probe --record-target <source>
```

```bash
python3 tools/roundtrip.py analyze before.wav -l before -o before.json
```

Repeat after patching to get `after.json`, then:

```bash
python3 tools/roundtrip.py chart --before before.json --after after.json -o roundtrip.png
```

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

The analysis is validated against synthetic recordings with known properties —
a mono, band-limited, high-passed path versus a stereo, full-band, unfiltered
one. It recovers the injected delays exactly and separates the two cases
cleanly:

![self-test](roundtrip-selftest.png)

This chart is a test of the measurement code. It is **not** a Discord
measurement, and no numbers in this repository are presented as one.
