//! The README chart: one strip per scheme, one column per flash operation of
//! the device's life, coloured by the worst thing a power cut there led to.

use crate::life::{Event, Outcome};
use crate::search::Report;
use std::fmt::Write;

const INK: &str = "#24292f";
const MUTED: &str = "#57606a";
const FONT: &str = "-apple-system, 'Segoe UI', Helvetica, Arial, sans-serif";

fn colour(o: &Outcome) -> &'static str {
    match o {
        Outcome::Ok => "#8fd19e",
        Outcome::Stuck(_) => "#d4a72c",
        Outcome::Bricked(_) => "#cf222e",
        Outcome::Unsafe(_) => "#8250df",
    }
}

/// A stretch of the uncut life, named for what it did.
struct Phase {
    from: usize,
    to: usize,
    label: String,
}

fn phases(events: &[Event], total: usize) -> Vec<Phase> {
    let mut out: Vec<Phase> = Vec::new();
    let mut start = 0;
    for e in events {
        let label = if let Some(rest) = e.what.strip_prefix("boot ") {
            rest.split(' ').next().map(|v| format!("boot {v}"))
        } else if e.what.contains("written") {
            e.what.split(' ').next().map(|v| format!("write {v}"))
        } else if e.what.ends_with("confirmed") {
            Some(format!(
                "confirm {}",
                e.what.split(' ').next().unwrap_or("")
            ))
        } else {
            None
        };
        if let Some(label) = label {
            if e.op > start {
                out.push(Phase {
                    from: start,
                    to: e.op,
                    label,
                });
            }
            start = e.op;
        }
    }
    if start < total {
        out.push(Phase {
            from: start,
            to: total,
            label: String::new(),
        });
    }
    out
}

/// `rows` pairs each report with the events of its uncut life.
pub fn chart(title: &str, subtitle: &str, rows: &[(&Report, &[Event])]) -> String {
    let (left, right, top, strip, gap) = (200.0, 24.0, 84.0, 30.0, 50.0);
    let width = 1000.0;
    let plot = width - left - right;
    let height = top + rows.len() as f64 * (strip + gap) + 40.0;
    let mut s = String::new();
    let _ = writeln!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="{FONT}">"##
    );
    let _ = writeln!(s, r##"<rect width="100%" height="100%" fill="#ffffff"/>"##);
    let _ = writeln!(
        s,
        r##"<text x="24" y="30" font-size="17" font-weight="600" fill="{INK}">{}</text>"##,
        esc(title)
    );
    let _ = writeln!(
        s,
        r##"<text x="24" y="50" font-size="12.5" fill="{MUTED}">{}</text>"##,
        esc(subtitle)
    );

    for (i, (r, events)) in rows.iter().enumerate() {
        let y = top + i as f64 * (strip + gap);
        let worst = r.worst_by_op();
        let n = worst.len().max(1);
        let w = plot / n as f64;
        let x = |op: usize| left + op as f64 * w;
        let failed = r.failures();
        let _ = writeln!(
            s,
            r##"<text x="{}" y="{}" font-size="13" font-weight="600" text-anchor="end" fill="{INK}">{}</text>"##,
            left - 10.0,
            y + 13.0,
            esc(&r.scheme)
        );
        let pct = 100.0 * failed as f64 / r.trials.len().max(1) as f64;
        let _ = writeln!(
            s,
            r##"<text x="{}" y="{}" font-size="11.5" text-anchor="end" fill="{MUTED}">{} of {} cuts fail ({:.1}%)</text>"##,
            left - 10.0,
            y + 28.0,
            failed,
            r.trials.len(),
            pct
        );
        for (op, o) in worst.iter().enumerate() {
            let _ = writeln!(
                s,
                r##"<rect x="{:.2}" y="{y}" width="{:.2}" height="{strip}" fill="{}"/>"##,
                x(op),
                (w - 0.6).max(0.6),
                colour(o)
            );
        }
        for p in phases(events, n) {
            let (a, b) = (x(p.from), x(p.to));
            let _ = writeln!(
                s,
                r##"<line x1="{a:.1}" y1="{}" x2="{a:.1}" y2="{}" stroke="{INK}" stroke-width="1"/>"##,
                y + strip,
                y + strip + 6.0
            );
            if b - a > 34.0 && !p.label.is_empty() {
                let _ = writeln!(
                    s,
                    r##"<text x="{:.1}" y="{}" font-size="10.5" text-anchor="middle" fill="{MUTED}">{}</text>"##,
                    (a + b) / 2.0,
                    y + strip + 16.0,
                    esc(&p.label)
                );
            }
        }
    }

    let ly = height - 22.0;
    let mut lx = left;
    for (o, label) in [
        (Outcome::Ok, "boots and finishes the update"),
        (Outcome::Bricked(String::new()), "bricked"),
        (Outcome::Stuck(String::new()), "stuck: never settles"),
        (
            Outcome::Unsafe(String::new()),
            "boots something it must not",
        ),
    ] {
        let _ = writeln!(
            s,
            r##"<rect x="{lx}" y="{}" width="14" height="14" fill="{}"/>"##,
            ly - 11.0,
            colour(&o)
        );
        let _ = writeln!(
            s,
            r##"<text x="{}" y="{ly}" font-size="12" fill="{MUTED}">{}</text>"##,
            lx + 20.0,
            esc(label)
        );
        lx += 20.0 + 6.4 * label.len() as f64 + 26.0;
    }
    s.push_str("</svg>\n");
    s
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
