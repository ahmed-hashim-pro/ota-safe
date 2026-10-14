use ota_sim::flash::Cut;
use ota_sim::life::{live, live_keeping_cut, Life, Outcome, Workload};
use ota_sim::scheme::{self, Images, NaiveAB, NaiveInPlace, OtaSafe, Scheme, MUTANTS};
use ota_sim::search::{fresh, search, Report};
use std::process::ExitCode;

const USAGE: &str = "ota-sim: cut power at every flash operation of a firmware update

usage:
  ota-sim matrix  [--workload good|bad-update|both] [--depth 1|2] [--threads N]
      every power cut, for naive in-place, naive A/B and ota-safe
  ota-sim mutants [--threads N]
      the same search against ota-safe with each planted bug
  ota-sim life    [--scheme NAME] [--workload W] [--cut AT[:TEAR]]...
      one device life, event by event; a second --cut fires after power returns
  ota-sim dump    [--scheme NAME] [--workload W] --cut AT[:TEAR] -o FILE
      the flash image the moment power is cut, and what the bootloader makes of it
  ota-sim states  --dir DIR [--workload W]
      every depth-1 cut state of ota-safe as a flash image, plus expected.txt:
      what the bootloader decides for each (`make qemu` checks it on a Cortex-M3)
  ota-sim key
      the simulator's Ed25519 public key
  ota-sim chart   -o FILE
      the README chart

schemes:  naive-in-place, naive-ab, ota-safe, or a mutant name (see `ota-sim mutants`)
tears:    before, prefix-1, prefix-2, prefix-3, scatter (program); before, erase-0, erase-1 (erase)
exit:     matrix and mutants exit 1 if ota-safe fails a cut or a planted bug goes uncaught";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let opts = match Opts::parse(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("ota-sim: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r = match cmd.as_str() {
        "matrix" => matrix(&opts),
        "mutants" => mutants(&opts),
        "life" => life(&opts),
        "dump" => dump(&opts),
        "chart" => chart(&opts),
        "states" => states(&opts),
        "key" => {
            let k = scheme::verifying_key().to_bytes();
            println!(
                "{}",
                k.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            Ok(true)
        }
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(true)
        }
        _ => Err(format!("unknown command {cmd:?}")),
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("ota-sim: {e}");
            ExitCode::from(2)
        }
    }
}

struct Opts {
    workloads: Vec<Workload>,
    depth: usize,
    threads: usize,
    scheme: String,
    cuts: Vec<Cut>,
    out: Option<String>,
    dir: Option<String>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Opts, String> {
        let mut o = Opts {
            workloads: vec![Workload::Good, Workload::BadUpdate],
            depth: 1,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            scheme: "ota-safe".into(),
            cuts: Vec::new(),
            out: None,
            dir: None,
        };
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
            match a.as_str() {
                "--workload" => {
                    let v = val()?;
                    o.workloads = if v == "both" {
                        vec![Workload::Good, Workload::BadUpdate]
                    } else {
                        vec![v.parse()?]
                    };
                }
                "--depth" => {
                    o.depth = val()?.parse().map_err(|_| "--depth takes 1 or 2")?;
                    if !(1..=2).contains(&o.depth) {
                        return Err("--depth takes 1 or 2".into());
                    }
                }
                "--threads" => {
                    o.threads = val()?.parse().map_err(|_| "--threads takes a number")?
                }
                "--scheme" => o.scheme = val()?,
                "--cut" => o.cuts.push(val()?.parse()?),
                "-o" | "--out" => o.out = Some(val()?),
                "--dir" => o.dir = Some(val()?),
                _ => return Err(format!("unknown option {a:?}")),
            }
        }
        Ok(o)
    }

    fn workload(&self) -> Workload {
        self.workloads[0]
    }
}

fn scheme_by_name(name: &str) -> Result<Box<dyn Scheme>, String> {
    Ok(match name {
        "naive-in-place" => Box::new(NaiveInPlace),
        "naive-ab" => Box::new(NaiveAB),
        "ota-safe" => Box::new(OtaSafe::new()),
        _ => {
            let m = MUTANTS
                .iter()
                .find(|m| scheme::mutant_name(**m) == name)
                .ok_or(format!("unknown scheme {name:?}"))?;
            Box::new(OtaSafe::with_mutant(*m))
        }
    })
}

fn describe(w: Workload) -> &'static str {
    match w {
        Workload::Good => "v1 installs v2, v2 installs v3; every version healthy",
        Workload::BadUpdate => "the same, but v3 crashes before it can confirm itself",
    }
}

fn row(r: &Report) -> String {
    let n = r.trials.len();
    let verdict = if r.failures() == 0 {
        "every cut ok".to_string()
    } else {
        ["BRICKED", "STUCK", "UNSAFE"]
            .into_iter()
            .filter(|l| r.count(l) > 0)
            .map(|l| {
                format!(
                    "{:.1}% {}",
                    100.0 * r.count(l) as f64 / n as f64,
                    l.to_lowercase()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{:<16} {:>8} {:>9} {:>10} {:>8} {:>8} {:>8} {:>8}   {}",
        r.scheme,
        r.uncut.label(),
        r.ops.len(),
        n,
        r.count("ok"),
        r.count("BRICKED"),
        r.count("STUCK"),
        r.count("UNSAFE"),
        verdict
    )
}

fn matrix(o: &Opts) -> Result<bool, String> {
    let images = Images::standard();
    let schemes: [Box<dyn Scheme>; 3] = [
        Box::new(NaiveInPlace),
        Box::new(NaiveAB),
        Box::new(OtaSafe::new()),
    ];
    let mut ok = true;
    let start = std::time::Instant::now();
    for (i, &w) in o.workloads.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}: {}", w.name(), describe(w));
        println!(
            "{:<16} {:>8} {:>9} {:>10} {:>8} {:>8} {:>8} {:>8}",
            "scheme",
            "no cut",
            "flash ops",
            if o.depth == 2 { "1-2 cuts" } else { "cuts" },
            "ok",
            "bricked",
            "stuck",
            "unsafe"
        );
        for s in &schemes {
            let r = search(s.as_ref(), &images, w, o.depth, o.threads);
            println!("{}", row(&r));
            if r.scheme == "ota-safe" && (r.failures() > 0 || r.uncut != Outcome::Ok) {
                ok = false;
                if let Some(t) = r.first_failure() {
                    println!("  first: --cut {}  {}", cut_args(&t.cuts), t.outcome);
                }
            }
        }
    }
    println!(
        "\ndepth {}. A cut is one erase or program interrupted one way; replay one with:\n  ota-sim life --scheme NAME --workload W --cut AT:TEAR",
        o.depth
    );
    eprintln!("({:.1}s)", start.elapsed().as_secs_f64());
    Ok(ok)
}

fn cut_args(cuts: &[Cut]) -> String {
    cuts.iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" --cut ")
}

fn mutants(o: &Opts) -> Result<bool, String> {
    let images = Images::standard();
    println!(
        "{:<24} {:<10} {:>8} {:>13}   first failure",
        "planted bug", "result", "no cut", "failing cuts"
    );
    let mut all = true;
    {
        let (fails, total, first, uncut) = run_both(&OtaSafe::new(), &images, o.threads);
        let name = "none (ota-safe)";
        let res = if fails == 0 { "clean" } else { "FAILED" };
        all &= fails == 0;
        println!(
            "{:<24} {:<10} {:>8} {:>13}   {}",
            name,
            res,
            uncut,
            format!("{fails}/{total}"),
            first
        );
    }
    for m in MUTANTS {
        let s = OtaSafe::with_mutant(m);
        let (fails, total, first, uncut) = run_both(&s, &images, o.threads);
        let res = if fails > 0 || uncut != "ok" {
            "caught"
        } else {
            "MISSED"
        };
        all &= res == "caught";
        println!(
            "{:<24} {:<10} {:>8} {:>13}   {}",
            scheme::mutant_name(m),
            res,
            uncut,
            format!("{fails}/{total}"),
            first
        );
    }
    println!(
        "\nboth workloads, depth 1. Replay one: ota-sim life --scheme NAME --workload W [--cut AT:TEAR]"
    );
    Ok(all)
}

/// Failing cuts over both workloads, the total, the first failure, and how
/// the uncut lives ended.
fn run_both(
    s: &dyn Scheme,
    images: &Images,
    threads: usize,
) -> (usize, usize, String, &'static str) {
    let mut fails = 0;
    let mut total = 0;
    let mut first = String::new();
    let mut uncut = "ok";
    for w in [Workload::Good, Workload::BadUpdate] {
        let r = search(s, images, w, 1, threads);
        fails += r.failures();
        total += r.trials.len();
        if r.uncut != Outcome::Ok && uncut == "ok" {
            uncut = r.uncut.label();
            first = format!("{} no cut: {}", w.name(), r.uncut);
        }
        if first.is_empty() {
            if let Some(t) = r.first_failure() {
                first = format!("{} {}: {}", w.name(), cut_args(&t.cuts), t.outcome);
            }
        }
    }
    if first.is_empty() {
        first = "-".into();
    }
    (fails, total, first, uncut)
}

fn print_life(l: &Life) {
    let mut period = usize::MAX;
    for e in &l.events {
        if e.period != period {
            period = e.period;
            println!(
                "power on{}",
                if period == 0 {
                    String::new()
                } else {
                    format!(" (#{})", period + 1)
                }
            );
        }
        println!("  op {:>4}  {}", e.op, e.what);
    }
    println!("{}", l.outcome);
}

fn life(o: &Opts) -> Result<bool, String> {
    let images = Images::standard();
    let s = scheme_by_name(&o.scheme)?;
    let f = fresh(s.as_ref(), &images);
    let l = live(s.as_ref(), &images, &f, o.workload(), &o.cuts);
    println!(
        "{} / {} workload{}",
        s.name(),
        o.workload().name(),
        if o.cuts.is_empty() {
            String::new()
        } else {
            format!(" / cut {}", cut_args(&o.cuts).replace(" --cut ", ", then "))
        }
    );
    print_life(&l);
    Ok(l.outcome == Outcome::Ok)
}

/// The flash at the moment power goes, before anything recovers it.
fn dump(o: &Opts) -> Result<bool, String> {
    let out = o.out.as_ref().ok_or("dump needs -o FILE")?;
    let [cut] = o.cuts[..] else {
        return Err("dump takes exactly one --cut".into());
    };
    let images = Images::standard();
    let s = scheme_by_name(&o.scheme)?;
    let f = fresh(s.as_ref(), &images);
    let l = live_keeping_cut(s.as_ref(), &images, &f, o.workload(), &[cut]);
    let mut snap = l
        .at_first_cut
        .ok_or(format!("the life has no flash operation {}", cut.at))?;
    std::fs::write(out, &snap.mem).map_err(|e| format!("{out}: {e}"))?;
    snap.power_on();
    let b = s
        .boot(&mut snap)
        .map_err(|_| "power loss during boot".to_string())?;
    println!(
        "{out}: {} bytes; {} would boot: {b:?}",
        snap.mem.len(),
        s.name()
    );
    Ok(true)
}

fn chart(o: &Opts) -> Result<bool, String> {
    let out = o.out.as_ref().ok_or("chart needs -o FILE")?;
    let images = Images::standard();
    let schemes: [Box<dyn Scheme>; 3] = [
        Box::new(NaiveInPlace),
        Box::new(NaiveAB),
        Box::new(OtaSafe::new()),
    ];
    let reports: Vec<Report> = schemes
        .iter()
        .map(|s| search(s.as_ref(), &images, Workload::Good, 1, o.threads))
        .collect();
    let lives: Vec<Life> = schemes
        .iter()
        .map(|s| {
            live(
                s.as_ref(),
                &images,
                &fresh(s.as_ref(), &images),
                Workload::Good,
                &[],
            )
        })
        .collect();
    let rows: Vec<(&Report, &[ota_sim::life::Event])> = reports
        .iter()
        .zip(&lives)
        .map(|(r, l)| (r, &l.events[..]))
        .collect();
    let svg = ota_sim::svg::chart(
        "A power cut at every flash operation of two firmware updates",
        "Each column is one erase or program in the device's life, v1 to v3; its colour is the worst result of cutting power there, over every way the operation can be left torn.",
        &rows,
    );
    std::fs::write(out, svg).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out}");
    Ok(true)
}

/// What the ota-safe bootloader decides for a flash image, and the metadata
/// record it leaves, in the same words the QEMU build prints.
pub fn decide(f: &mut ota_sim::flash::SimFlash) -> String {
    let layout = ota_safe::Layout::standard();
    let key = scheme::verifying_key();
    let cfg = ota_safe::Config::default();
    let mut s = match ota_safe::boot(f, &layout, &key, &cfg) {
        Ok(ota_safe::Boot::Slot {
            slot,
            version,
            trial,
        }) => format!("boot slot={slot:?} version={version} trial={trial}"),
        Ok(ota_safe::Boot::Brick(b)) => format!("brick {b:?}"),
        Err(_) => "power loss".into(),
    };
    match ota_safe::read_meta(f, &layout, &cfg) {
        Ok(Some((m, _))) => s.push_str(&format!(" meta={m:?}")),
        _ => s.push_str(" meta=None"),
    }
    s
}

fn states(o: &Opts) -> Result<bool, String> {
    let dir = o.dir.as_ref().ok_or("states needs --dir DIR")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    let images = Images::standard();
    let s = OtaSafe::new();
    let f = fresh(&s, &images);
    let mut expected = String::new();
    let mut n = 0;
    for &w in &o.workloads {
        let uncut = live(&s, &images, &f, w, &[]);
        let ops = uncut.periods.concat();
        for (at, op) in ops.iter().enumerate() {
            for &tear in ota_sim::flash::Tear::for_kind(op.kind) {
                let cut = Cut { at, tear };
                let l = live_keeping_cut(&s, &images, &f, w, &[cut]);
                let mut snap = l.at_first_cut.ok_or("cut did not fire")?;
                let name = format!("{}-{:03}-{}.bin", w.name(), at, tear);
                std::fs::write(format!("{dir}/{name}"), &snap.mem)
                    .map_err(|e| format!("{name}: {e}"))?;
                snap.power_on();
                expected.push_str(&format!("{name}\t{}\n", decide(&mut snap)));
                n += 1;
            }
        }
    }
    std::fs::write(format!("{dir}/expected.txt"), expected).map_err(|e| e.to_string())?;
    println!("wrote {n} flash images and expected.txt to {dir}");
    Ok(true)
}
