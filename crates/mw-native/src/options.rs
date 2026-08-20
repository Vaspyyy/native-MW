use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

pub const DEFAULT_SCENARIO: &str = "../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz";
pub const DEFAULT_RUNTIME_TICK_MS: u64 = 33;
pub const DEFAULT_RUNTIME_QUEUE_CAPACITY: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppOptions {
    pub scenario_path: PathBuf,
    pub runtime_checkpoint_path: Option<PathBuf>,
    pub demo_units: bool,
    pub smoke_frames: Option<u64>,
    pub runtime_tick_interval: Duration,
    pub runtime_queue_capacity: usize,
    pub headless_ticks: Option<u64>,
    pub json: bool,
    pub show_help: bool,
}

pub fn parse_app_options(arguments: impl IntoIterator<Item = OsString>) -> Result<AppOptions> {
    let mut arguments = arguments.into_iter();
    let mut scenario_path = None;
    let mut runtime_checkpoint_path = None;
    let mut demo_units = false;
    let mut smoke_frames = None;
    let mut runtime_tick_ms = DEFAULT_RUNTIME_TICK_MS;
    let mut runtime_queue_capacity = DEFAULT_RUNTIME_QUEUE_CAPACITY;
    let mut runtime_tuning_requested = false;
    let mut headless = false;
    let mut headless_ticks = 5_u64;
    let mut ticks_requested = false;
    let mut json = false;
    let mut show_help = false;

    while let Some(argument) = arguments.next() {
        let flag = argument.to_string_lossy();
        match flag.as_ref() {
            "--smoke" => smoke_frames = Some(3),
            "--demo-units" => demo_units = true,
            "--runtime-checkpoint" | "--checkpoint" => {
                if runtime_checkpoint_path.is_some() {
                    bail!("runtime checkpoint was supplied more than once");
                }
                runtime_checkpoint_path = Some(PathBuf::from(
                    arguments
                        .next()
                        .context("--runtime-checkpoint needs a JSON path")?,
                ));
            }
            "--tick-ms" => {
                runtime_tick_ms = parse_positive::<u64>(
                    arguments.next().context("--tick-ms needs a value")?,
                    "--tick-ms",
                )?;
                runtime_tuning_requested = true;
            }
            "--update-queue" => {
                runtime_queue_capacity = parse_positive::<usize>(
                    arguments.next().context("--update-queue needs a value")?,
                    "--update-queue",
                )?;
                runtime_tuning_requested = true;
            }
            "--headless" => headless = true,
            "--ticks" => {
                headless_ticks = parse_positive::<u64>(
                    arguments.next().context("--ticks needs a value")?,
                    "--ticks",
                )?;
                ticks_requested = true;
            }
            "--json" => json = true,
            "--help" | "-h" => show_help = true,
            _ if flag.starts_with('-') => bail!("unknown option {flag:?}"),
            _ if scenario_path.is_none() => scenario_path = Some(PathBuf::from(argument)),
            _ => bail!("unexpected argument {argument:?}"),
        }
    }

    if demo_units && runtime_checkpoint_path.is_some() {
        bail!("--demo-units and --runtime-checkpoint are mutually exclusive");
    }
    if runtime_tuning_requested && !demo_units && runtime_checkpoint_path.is_none() {
        bail!("--tick-ms and --update-queue require a native runtime mode");
    }
    if headless && runtime_checkpoint_path.is_none() {
        bail!("--headless requires --runtime-checkpoint");
    }
    if headless && (demo_units || smoke_frames.is_some()) {
        bail!("--headless is mutually exclusive with --demo-units and --smoke");
    }
    if (ticks_requested || json) && !headless {
        bail!("--ticks and --json require --headless");
    }

    Ok(AppOptions {
        scenario_path: scenario_path.unwrap_or_else(|| PathBuf::from(DEFAULT_SCENARIO)),
        runtime_checkpoint_path,
        demo_units,
        smoke_frames,
        runtime_tick_interval: Duration::from_millis(runtime_tick_ms),
        runtime_queue_capacity,
        headless_ticks: headless.then_some(headless_ticks),
        json,
        show_help,
    })
}

pub fn help_text() -> &'static str {
    "mw-native [OPTIONS] [SCENARIO.mwsc.gz]\n\
     \n\
     Options:\n\
       --runtime-checkpoint PATH  Load a strict browser postStartWar checkpoint\n\
       --demo-units               Run the small scenario-derived demo runtime\n\
       --tick-ms N                Runtime tick interval in milliseconds (default 33)\n\
       --update-queue N           Bounded lossless publication queue (default 8)\n\
       --headless                 Run the checkpoint worker without a window\n\
       --ticks N                  Exact headless worker steps (default 5)\n\
       --json                     Emit the headless result as JSON\n\
       --smoke                    Present three frames (and one runtime tick) then exit\n\
       -h, --help                 Show this help"
}

fn parse_positive<T>(value: OsString, flag: &str) -> Result<T>
where
    T: std::str::FromStr + PartialEq + Default,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let parsed = value
        .to_str()
        .with_context(|| format!("{flag} must be valid UTF-8"))?
        .parse::<T>()
        .with_context(|| format!("invalid {flag} value"))?;
    if parsed == T::default() {
        bail!("{flag} must be greater than zero");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<AppOptions> {
        parse_app_options(arguments.iter().map(OsString::from))
    }

    #[test]
    fn defaults_to_map_only_viewer() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.scenario_path, PathBuf::from(DEFAULT_SCENARIO));
        assert_eq!(options.runtime_tick_interval, Duration::from_millis(33));
        assert_eq!(options.runtime_queue_capacity, 8);
        assert!(options.runtime_checkpoint_path.is_none());
        assert!(!options.demo_units);
        assert!(options.headless_ticks.is_none());
    }

    #[test]
    fn parses_checkpoint_and_thread_tuning() {
        let options = parse(&[
            "--runtime-checkpoint",
            "checkpoint.json",
            "--tick-ms",
            "20",
            "--update-queue",
            "4",
            "scenario.mwsc.gz",
            "--smoke",
        ])
        .unwrap();
        assert_eq!(
            options.runtime_checkpoint_path,
            Some(PathBuf::from("checkpoint.json"))
        );
        assert_eq!(options.scenario_path, PathBuf::from("scenario.mwsc.gz"));
        assert_eq!(options.runtime_tick_interval, Duration::from_millis(20));
        assert_eq!(options.runtime_queue_capacity, 4);
        assert_eq!(options.smoke_frames, Some(3));
    }

    #[test]
    fn rejects_conflicts_zeroes_and_unused_tuning() {
        assert!(parse(&["--demo-units", "--checkpoint", "x.json"]).is_err());
        assert!(parse(&["--demo-units", "--tick-ms", "0"]).is_err());
        assert!(parse(&["--runtime-checkpoint", "x.json", "--update-queue", "0"]).is_err());
        assert!(parse(&["--tick-ms", "10"]).is_err());
        assert!(parse(&["--headless"]).is_err());
        assert!(parse(&["--runtime-checkpoint", "x.json", "--ticks", "2"]).is_err());
        assert!(parse(&["--unknown"]).is_err());
    }

    #[test]
    fn parses_exact_headless_worker_run() {
        let options = parse(&[
            "--runtime-checkpoint",
            "checkpoint.json",
            "--headless",
            "--ticks",
            "7",
            "--json",
        ])
        .unwrap();
        assert_eq!(options.headless_ticks, Some(7));
        assert!(options.json);
        assert!(options.smoke_frames.is_none());
    }
}
