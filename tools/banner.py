#!/usr/bin/env python3
"""Render the project's social preview banner.

GitHub shows this image wherever the repository is linked, at sizes down to a
thumbnail, so it carries one idea rather than a summary: the same signal in
both channels versus two channels that differ. The lanes reuse the colours and
the solid-L / dashed-R convention of the measurement charts, and the numbers
under them are the ones docs/roundtrip.png actually measured.

    python3 tools/banner.py [-o docs/social-preview.png]

Generated rather than drawn by hand so the numbers cannot drift away from the
measurement they quote.

stereocord - Copyright (c) 2026 Paul Neri
<67437654+DasPauluteli@users.noreply.github.com>

Licensed under CC BY-NC-SA 4.0: non-commercial use, share alike, keep this
notice, no patent grant. See LICENSE, or
https://creativecommons.org/licenses/by-nc-sa/4.0/

SPDX-License-Identifier: CC-BY-NC-SA-4.0
"""

import argparse
import json
import os

import numpy as np

W, H = 1280, 640
DPI = 100

BG_TOP = "#0d1117"
BG_BOTTOM = "#161b22"
INK = "#e6edf3"
MUTED = "#8b949e"
FAINT = "#30363d"
RED = "#e5534b"
BLUE = "#4c8eff"
RIGHT = "#f2f6fa"

SANS = ["Fira Sans", "Adwaita Sans", "Noto Sans", "DejaVu Sans"]
MONO = ["Adwaita Mono", "DejaVu Sans Mono", "monospace"]

# Fallbacks for the quoted measurement, used when the capture's JSON is not
# checked out. They are the numbers behind docs/roundtrip.png.
BEFORE_CORR, AFTER_CORR = 1.0, -0.004


def measured(root):
    """The correlation figures, read from the capture if it is present."""
    out = []
    for name, fallback in (("before.json", BEFORE_CORR), ("after.json", AFTER_CORR)):
        path = os.path.join(root, name)
        try:
            with open(path) as f:
                out.append(float(json.load(f)["channel_correlation"]))
        except (OSError, KeyError, ValueError):
            out.append(fallback)
    return out


def wiggle(n, seed, scale=1.0):
    """A smooth, audio-looking trace. Decorative: this is not a recording."""
    rng = np.random.default_rng(seed)
    x = np.linspace(0, 1, n)
    y = np.zeros(n)
    for k in range(1, 7):
        y += rng.normal(0, 1 / k) * np.sin(2 * np.pi * (k * 2.2) * x + rng.uniform(0, 6.3))
    y *= np.hanning(n) ** 0.35
    return scale * y / np.abs(y).max()


LANE_X0, LANE_X1 = 764, 1200


def lane(fig, ax, y0, colour, title, left, right, number, verdict):
    """One channel pair: solid left, dashed right, on a hairline baseline.

    The right channel is always the pale dashed trace. When it lies exactly on
    the left one - which is what a mono downmix looks like - the dashes stay
    visible against the colour underneath, so the pair still reads as two
    channels carrying one signal rather than as a single line.
    """
    x = np.linspace(LANE_X0, LANE_X1, len(left))
    ax.plot([LANE_X0, LANE_X1], [y0, y0], color=FAINT, lw=1, zorder=1)
    ax.plot(x, y0 - left, color=colour, lw=3.0, zorder=2)
    ax.plot(x, y0 - right, color=RIGHT, lw=1.8, ls=(0, (6, 5)), zorder=3)

    base = y0 - 84
    ax.text(LANE_X0, base, title, color=colour, fontsize=18, fontfamily=SANS,
            fontweight="bold", va="baseline")
    tag = ax.text(LANE_X1, base, verdict, color=colour, fontsize=18,
                  fontfamily=SANS, fontweight="bold", va="baseline", ha="right")

    # Park the number to the left of the verdict, whatever width it renders at.
    fig.canvas.draw()
    width = tag.get_window_extent(fig.canvas.get_renderer()).width
    ax.text(LANE_X1 - width - 18, base, number, color=MUTED, fontsize=15,
            fontfamily=MONO, va="baseline", ha="right")


def render(out_path, root):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.colors import LinearSegmentedColormap
    from matplotlib.patches import Rectangle

    before_corr, after_corr = measured(root)

    fig = plt.figure(figsize=(W / DPI, H / DPI), dpi=DPI)
    ax = fig.add_axes([0, 0, 1, 1])
    ax.set_xlim(0, W)
    ax.set_ylim(H, 0)
    ax.axis("off")

    grad = LinearSegmentedColormap.from_list("bg", [BG_TOP, BG_BOTTOM])
    ax.imshow(np.linspace(0, 1, 256).reshape(-1, 1), cmap=grad,
              extent=(0, W, H, 0), aspect="auto", zorder=0)

    # A cool wash behind the patched lane, so the eye lands on the right half.
    glow = np.linspace(0, 1, 256).reshape(1, -1) ** 3
    ax.imshow(glow, cmap=LinearSegmentedColormap.from_list("g", [BG_BOTTOM, "#1b2b45"]),
              extent=(640, W, H, 0), aspect="auto", alpha=0.55, zorder=0)

    ax.add_patch(Rectangle((0, 0), W, 6, color=RED, zorder=5))
    ax.add_patch(Rectangle((W * 0.5, 0), W * 0.5, 6, color=BLUE, zorder=6))

    ax.text(80, 236, "stereocord", color=INK, fontsize=72, fontfamily=SANS,
            fontweight="bold", va="baseline")
    ax.text(84, 288, "True stereo, 48 kHz and", color=INK,
            fontsize=24, fontfamily=SANS, va="baseline")
    ax.text(84, 322, "high-bitrate Opus, inside", color=INK,
            fontsize=24, fontfamily=SANS, va="baseline")
    ax.text(84, 356, "Discord's Linux voice module.", color=MUTED,
            fontsize=24, fontfamily=SANS, va="baseline")

    ax.plot([84, 132], [400, 400], color=RED, lw=3, solid_capstyle="butt")
    ax.plot([140, 188], [400, 400], color=BLUE, lw=3, solid_capstyle="butt")

    ax.text(84, 458, "patched in place, by signature", color=MUTED,
            fontsize=17, fontfamily=MONO, va="baseline")
    ax.text(84, 490, "nothing downloaded, every byte verified", color=MUTED,
            fontsize=17, fontfamily=MONO, va="baseline")

    n = 600
    mono = wiggle(n, 11, 44)
    lane(fig, ax, 248, RED, "stock", mono, mono,
         f"L/R {before_corr:.3f}", "mono")
    lane(fig, ax, 470, BLUE, "patched", wiggle(n, 3, 44), wiggle(n, 8, 44),
         f"L/R {after_corr:+.3f}", "stereo")

    ax.text(80, 578, "github.com/DasPauluteli/stereocord", color="#4a5058",
            fontsize=16, fontfamily=MONO, va="baseline")

    fig.savefig(out_path, dpi=DPI, facecolor=BG_TOP)
    plt.close(fig)
    print(f"wrote {out_path} ({W}x{H})")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(here)
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("-o", "--out", default=os.path.join(root, "docs", "social-preview.png"))
    args = ap.parse_args()
    render(args.out, root)


if __name__ == "__main__":
    main()
