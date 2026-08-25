use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use mw_core::GameDate;

pub const DEFAULT_SCENARIO: &str = "../modern-wars/assets/maps/compiled/world-map-2022-v2.mwsc.gz";
pub const DEFAULT_RUNTIME_TICK_MS: u64 = 33;
pub const DEFAULT_RUNTIME_QUEUE_CAPACITY: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppOptions {
    pub scenario_path: PathBuf,
    pub runtime_checkpoint_path: Option<PathBuf>,
    pub native_war_sides: Vec<Vec<String>>,
    pub start_date: Option<GameDate>,
    pub save_checkpoint_path: Option<PathBuf>,
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
    let mut native_war_sides = Vec::new();
    let mut start_date = None;
    let mut save_checkpoint_path = None;
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
            "--side" => {
                let value = arguments
                    .next()
                    .context("--side needs COUNTRY[,COUNTRY...]")?;
                let selectors = value
                    .to_str()
                    .context("--side must be valid UTF-8")?
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if selectors.iter().any(|selector| selector.is_empty()) {
                    bail!("--side contains an empty country selector");
                }
                native_war_sides.push(selectors);
            }
            "--start-date" => {
                if start_date.is_some() {
                    bail!("--start-date was supplied more than once");
                }
                start_date = Some(parse_start_date(
                    arguments.next().context("--start-date needs YYYY-MM-DD")?,
                )?);
            }
            "--runtime-checkpoint" | "--checkpoint" => {
                if runtime_checkpoint_path.is_some() {
                    bail!("runtime checkpoint was supplied more than once");
                }
                let path = arguments
                    .next()
                    .context("--runtime-checkpoint needs a JSON path")?;
                if path.to_string_lossy().trim().is_empty() {
                    bail!("--runtime-checkpoint path must not be empty");
                }
                runtime_checkpoint_path = Some(PathBuf::from(path));
            }
            "--save-checkpoint" => {
                if save_checkpoint_path.is_some() {
                    bail!("save checkpoint path was supplied more than once");
                }
                let path = arguments
                    .next()
                    .context("--save-checkpoint needs a JSON path")?;
                if path.to_string_lossy().trim().is_empty() {
                    bail!("--save-checkpoint path must not be empty");
                }
                save_checkpoint_path = Some(PathBuf::from(path));
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

    let native_war_requested = !native_war_sides.is_empty();
    if native_war_requested && native_war_sides.len() < 2 {
        bail!("--side requires at least two side arguments");
    }
    if native_war_requested && (demo_units || runtime_checkpoint_path.is_some()) {
        bail!("--side, --demo-units, and --runtime-checkpoint are mutually exclusive");
    }
    if demo_units && runtime_checkpoint_path.is_some() {
        bail!("--demo-units and --runtime-checkpoint are mutually exclusive");
    }
    if start_date.is_some() && !(native_war_requested || demo_units) {
        bail!("--start-date requires --demo-units or at least two --side arguments");
    }
    let runtime_mode = native_war_requested || demo_units || runtime_checkpoint_path.is_some();
    if runtime_tuning_requested && !runtime_mode {
        bail!("--tick-ms and --update-queue require a native runtime mode");
    }
    if headless && !native_war_requested && runtime_checkpoint_path.is_none() {
        bail!("--headless requires --runtime-checkpoint or at least two --side arguments");
    }
    if headless && (demo_units || smoke_frames.is_some()) {
        bail!("--headless is mutually exclusive with --demo-units and --smoke");
    }
    if (ticks_requested || json) && !headless {
        bail!("--ticks and --json require --headless");
    }
    if save_checkpoint_path.is_some() && !runtime_mode {
        bail!("--save-checkpoint requires a native runtime mode");
    }
    if save_checkpoint_path.is_some() && demo_units {
        bail!("--save-checkpoint is unavailable with --demo-units");
    }

    Ok(AppOptions {
        scenario_path: scenario_path.unwrap_or_else(|| PathBuf::from(DEFAULT_SCENARIO)),
        runtime_checkpoint_path,
        native_war_sides,
        start_date,
        save_checkpoint_path,
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
       --runtime-checkpoint PATH  Load a strict browser postStartWar or midWar checkpoint\n\
       --side COUNTRY[,COUNTRY...]  Add a side; at least two sides start a new war\n\
       --start-date YYYY-MM-DD   Enable the browser calendar for a new native war\n\
       --demo-units               Run the small scenario-derived demo runtime\n\
       --save-checkpoint PATH     Save resumable runtime state on exit/S; unavailable with --demo-units\n\
       --tick-ms N                Runtime tick interval in milliseconds (default 33)\n\
       --update-queue N           Bounded lossless publication queue (default 8)\n\
       --headless                 Run the checkpoint worker without a window\n\
       --ticks N                  Exact headless worker steps (default 5)\n\
       --json                     Emit the headless result as JSON\n\
       --smoke                    Present three frames (and one runtime tick) then exit\n\
       -h, --help                 Show this help"
}

fn parse_start_date(value: OsString) -> Result<GameDate> {
    let value = value.to_str().context("--start-date must be valid UTF-8")?;
    let mut parts = value.split('-');
    let year = parts
        .next()
        .context("--start-date needs YYYY-MM-DD")?
        .parse::<u32>()
        .context("invalid --start-date year")?;
    let month = parts
        .next()
        .context("--start-date needs YYYY-MM-DD")?
        .parse::<u8>()
        .context("invalid --start-date month")?;
    let day = parts
        .next()
        .context("--start-date needs YYYY-MM-DD")?
        .parse::<u8>()
        .context("invalid --start-date day")?;
    if parts.next().is_some() {
        bail!("--start-date needs exactly YYYY-MM-DD");
    }
    GameDate::new(year, month, day).map_err(|error| anyhow::anyhow!(error.to_string()))
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
        assert!(options.native_war_sides.is_empty());
        assert!(options.start_date.is_none());
        assert!(options.save_checkpoint_path.is_none());
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

    #[test]
    fn parses_new_war_sides_and_save_path() {
        let options = parse(&[
            "--side",
            "Germany,Italy",
            "--side",
            "France",
            "--start-date",
            "1939-09-01",
            "--save-checkpoint",
            "save.json",
            "--tick-ms",
            "20",
            "scenario.mwsc.gz",
        ])
        .unwrap();
        assert_eq!(
            options.native_war_sides,
            vec![vec!["Germany", "Italy"], vec!["France"]]
        );
        assert_eq!(options.start_date, Some(GameDate::new(1939, 9, 1).unwrap()));
        assert_eq!(
            options.save_checkpoint_path,
            Some(PathBuf::from("save.json"))
        );
        assert_eq!(options.scenario_path, PathBuf::from("scenario.mwsc.gz"));
    }

    #[test]
    fn rejects_new_war_and_save_conflicts() {
        assert!(parse(&["--side", "Germany"]).is_err());
        assert!(parse(&["--side", "Germany,", "--side", "France"]).is_err());
        assert!(parse(&["--side", "Germany", "--side", "France", "--demo-units"]).is_err());
        assert!(
            parse(&[
                "--side",
                "Germany",
                "--side",
                "France",
                "--checkpoint",
                "x.json"
            ])
            .is_err()
        );
        assert!(parse(&["--save-checkpoint", "save.json"]).is_err());
        let demo_save_error = parse(&["--demo-units", "--save-checkpoint", "save.json"])
            .unwrap_err()
            .to_string();
        assert!(demo_save_error.contains("unavailable with --demo-units"));
        assert!(help_text().contains("unavailable with --demo-units"));
        assert!(help_text().contains("--start-date YYYY-MM-DD"));
        assert!(parse(&["--start-date", "2024-01-01"]).is_err());
        assert!(parse(&["--demo-units", "--start-date", "1900-02-29"]).is_err());
        assert!(parse(&["--demo-units", "--start-date", "2000-02-29"]).is_ok());
        assert!(parse(&["--checkpoint", "x.json", "--start-date", "2024-01-01"]).is_err());
        assert!(
            parse(&[
                "--checkpoint",
                "x.json",
                "--save-checkpoint",
                "a",
                "--save-checkpoint",
                "b"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "--side",
                "Germany",
                "--side",
                "France",
                "--headless",
                "--ticks",
                "2"
            ])
            .is_ok()
        );
    }
}
