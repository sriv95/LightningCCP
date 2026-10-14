import io
from collections.abc import Sequence
from datetime import UTC, datetime

from matplotlib.figure import Figure

# (lowest rating, highest rating, colour) of the Codeforces rank bands.
CF_BANDS = (
    (0, 1200, "#cccccc"),
    (1200, 1400, "#77ff77"),
    (1400, 1600, "#77ddbb"),
    (1600, 1900, "#aaaaff"),
    (1900, 2100, "#ff88ff"),
    (2100, 2400, "#ffcc88"),
    (2400, 2600, "#ffbb55"),
    (2600, 3000, "#ff7777"),
    (3000, 3400, "#ff3333"),
    (3400, 4000, "#aa0000"),
)


def rating_plot(
    title: str,
    lines: Sequence[tuple[str, Sequence[tuple[int, int]]]],
    bands: Sequence[tuple[int, int, str]] = (),
) -> io.BytesIO:
    """PNG line chart of one or more (label, [(unix time, rating)]) lines, optionally over
    coloured rating bands. A legend shows when there is more than one line.

    Uses Figure directly, not pyplot, so it is safe to run in a worker thread."""
    fig = Figure(figsize=(9, 5), dpi=100)
    ax = fig.subplots()
    ratings = [r for _, points in lines for _, r in points]
    low, high = min(ratings) - 100, max(ratings) + 100
    for band_low, band_high, colour in bands:
        ax.axhspan(band_low, band_high, color=colour, alpha=0.45, linewidth=0)
    for label, points in lines:
        ax.plot(
            [datetime.fromtimestamp(t, UTC) for t, _ in points],
            [r for _, r in points],
            marker="o",
            markersize=4,
            linewidth=1.5,
            label=label,
            **({} if len(lines) > 1 else {"color": "black"}),
        )
    if len(lines) > 1:
        ax.legend(loc="best")
    ax.set_ylim(low, high)
    ax.set_title(title)
    ax.grid(alpha=0.3)
    fig.autofmt_xdate()
    buf = io.BytesIO()
    fig.savefig(buf, format="png")
    buf.seek(0)
    return buf
