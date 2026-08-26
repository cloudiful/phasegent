use crate::ci_model::{
    DEFAULT_INSPECT_POLL, DEFAULT_INSPECT_TIMEOUT, DEFAULT_LIMIT, DEFAULT_LOG_TAIL, DEFAULT_PAGE,
};
use crate::command::{self, Command, HelpTopic};

#[derive(Debug)]
pub enum CiCommand {
    Runs {
        sha: Option<String>,
        ref_name: Option<String>,
        status: Option<String>,
        workflow: Option<String>,
        page: usize,
        limit: usize,
    },
    RunGet {
        run_id: u64,
    },
    RunJobs {
        run_id: u64,
    },
    JobLogs {
        job_id: u64,
        tail: usize,
    },
    Inspect {
        sha: String,
        ref_name: Option<String>,
        wait: bool,
        timeout: u64,
        poll: u64,
    },
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let name = args.first().map(String::as_str);
    if name.is_none() || matches!(name, Some("--help" | "-h")) {
        return Ok(Command::Help(HelpTopic::Ci));
    }
    if args
        .iter()
        .skip(1)
        .any(|value| value == "--help" || value == "-h")
    {
        let nested = args
            .iter()
            .skip(1)
            .find(|value| *value != "--help" && *value != "-h")
            .map(String::as_str);
        let command = match (name.unwrap(), nested) {
            ("run", Some(value)) => format!("run {value}"),
            ("job", Some(value)) => format!("job {value}"),
            ("run", None) | ("job", None) => name.unwrap().to_owned(),
            (value, _) => value.to_owned(),
        };
        return Ok(Command::Help(HelpTopic::CiCommand(command)));
    }
    match name.unwrap() {
        "runs" => parse_runs(args),
        "run" => parse_run(args),
        "job" => parse_job(args),
        "inspect" => parse_inspect(args),
        value => Err(format!("unknown ci command '{value}'")),
    }
}

fn parse_runs(args: &[String]) -> Result<Command, String> {
    command::validate_options(
        args,
        0,
        &[
            "--sha",
            "--ref",
            "--status",
            "--workflow",
            "--page",
            "--limit",
        ],
        &[],
        "ci runs",
    )?;
    Ok(Command::Ci(CiCommand::Runs {
        sha: command::optional_option(args, "--sha"),
        ref_name: command::optional_option(args, "--ref"),
        status: command::optional_option(args, "--status"),
        workflow: command::optional_option(args, "--workflow"),
        page: positive_option(args, "--page", DEFAULT_PAGE, "ci runs")?,
        limit: positive_option(args, "--limit", DEFAULT_LIMIT, "ci runs")?,
    }))
}

fn parse_run(args: &[String]) -> Result<Command, String> {
    match args.get(1).map(String::as_str) {
        Some("get") => {
            command::require_exact_positionals(args, 3, "ci run get")?;
            Ok(Command::Ci(CiCommand::RunGet {
                run_id: numeric(args, 2, "ci run get")?,
            }))
        }
        Some("jobs") => {
            command::require_exact_positionals(args, 3, "ci run jobs")?;
            Ok(Command::Ci(CiCommand::RunJobs {
                run_id: numeric(args, 2, "ci run jobs")?,
            }))
        }
        Some(value) => Err(format!("unknown ci run command '{value}'")),
        None => Err("ci run requires get or jobs".to_owned()),
    }
}

fn parse_job(args: &[String]) -> Result<Command, String> {
    if args.get(1).map(String::as_str) != Some("logs") {
        return Err("ci job requires the logs subcommand".to_owned());
    }
    command::validate_options(args, 2, &["--tail"], &[], "ci job logs")?;
    Ok(Command::Ci(CiCommand::JobLogs {
        job_id: numeric(args, 2, "ci job logs")?,
        tail: numeric_option(args, "--tail", DEFAULT_LOG_TAIL as u64, "ci job logs")? as usize,
    }))
}

fn parse_inspect(args: &[String]) -> Result<Command, String> {
    command::validate_options(
        args,
        0,
        &["--sha", "--ref", "--timeout", "--poll"],
        &["--wait"],
        "ci inspect",
    )?;
    let sha = command::required_nonempty_option(args, "--sha", "ci inspect")?;
    Ok(Command::Ci(CiCommand::Inspect {
        sha,
        ref_name: command::optional_option(args, "--ref"),
        wait: command::has_flag(args, "--wait"),
        timeout: numeric_option(args, "--timeout", DEFAULT_INSPECT_TIMEOUT, "ci inspect")?,
        poll: numeric_option(args, "--poll", DEFAULT_INSPECT_POLL, "ci inspect")?,
    }))
}

fn positive_option(
    args: &[String],
    option: &str,
    default: usize,
    operation: &str,
) -> Result<usize, String> {
    let Some(value) = command::optional_option(args, option) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{operation} requires a numeric {option}"))?;
    if parsed == 0 {
        return Err(format!("{operation} requires {option} greater than zero"));
    }
    Ok(parsed)
}

fn numeric_option(
    args: &[String],
    option: &str,
    default: u64,
    operation: &str,
) -> Result<u64, String> {
    let Some(value) = command::optional_option(args, option) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{operation} requires a numeric {option}"))?;
    Ok(parsed)
}

fn numeric(args: &[String], index: usize, operation: &str) -> Result<u64, String> {
    args.get(index)
        .ok_or_else(|| format!("{operation} requires a numeric id"))?
        .parse()
        .map_err(|_| format!("{operation} requires a numeric id"))
}
