//! Command-line interface: the single configuration surface of the headless
//! gateway. No config file, no environment store, no wizard — the process
//! starts from `argv` alone (see `knowledge/decisions/headless-cli-gateway.md`).
//!
//! Parsing is hand-rolled: the flag set is five options plus `--help` /
//! `--version`, and the project keeps a deliberate zero-new-crates posture.
//! Everything is pure and unit-tested so validation behavior is provable
//! without spawning the binary.

use std::fmt;

/// Canonical NVIDIA NIM endpoint (the one the CLI proxies by default).
pub const DEFAULT_BASE_URL: &str = "https://integrate.api.nvidia.com";
/// Default listen address: loopback only. `--host 0.0.0.0` opts into
/// cross-container reachability deliberately, never by accident.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// Default listen port, matching the project's historical convention.
pub const DEFAULT_PORT: u16 = 8000;
/// Default rolling-window requests-per-minute budget. Deliberately
/// conservative vs. NVIDIA's ~40-RPM free tier so the safety margin
/// (see `pool::WINDOW`) has room to work.
pub const DEFAULT_RPM: u32 = 30;

/// Highest accepted `--rpm` value. Anything beyond this is a typo or a
/// misunderstanding (NIM's tier tops out far below it), and an unbounded
/// limiter state is not worth defending against.
pub const MAX_RPM: u32 = 10_000;

/// Parsed, validated startup parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub nim_api_key: String,
    pub rpm: u32,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    /// Default model injected into requests that omit `model` (from `--model`).
    pub default_model: Option<String>,
    /// Model forced onto EVERY request, overriding the client's choice
    /// (from `--force-model`). Mutually exclusive with `default_model`.
    pub force_model: Option<String>,
}

impl Args {
    /// The URL agents copy/paste as their OpenAI-compatible base URL.
    pub fn endpoint_url(&self) -> String {
        format!("http://{}:{}/v1", display_host(&self.host), self.port)
    }

    /// The effective model policy as a human-readable banner line fragment.
    pub fn model_policy(&self) -> String {
        match (&self.force_model, &self.default_model) {
            (Some(forced), _) => {
                format!("{forced} (pinned — every request is rewritten to this model)")
            }
            (None, Some(default)) => {
                format!("{default} (default — used when a request omits 'model')")
            }
            (None, None) => "passthrough (each client request names its own model)".into(),
        }
    }
}

/// Show `localhost` instead of an all-zeros bind address in the startup
/// banner: `http://0.0.0.0:8000/v1` is not a URL a client can use.
fn display_host(host: &str) -> &str {
    if host == "0.0.0.0" || host == "::" || host == "[::]" {
        "localhost"
    } else {
        host
    }
}

/// What `parse` decided to do with `argv`.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Start the server with these arguments.
    Run(Args),
    /// Print this text and exit successfully.
    Help(String),
    /// Print the version and exit successfully.
    Version(String),
    /// Invalid usage: print this text to stderr and exit with code 2.
    Error(String),
}

const HELP: &str = "\
nim-proxy — minimal headless OpenAI-compatible gateway for NVIDIA NIM

USAGE:
    nim-proxy --nim-api-key <KEY> [--rpm <RPM>] [--host <HOST>] [--port <PORT>]
              [--base-url <URL>] [--model <MODEL>] [--force-model <MODEL>]

OPTIONS:
    --nim-api-key <KEY>   NVIDIA NIM API key (nvapi-...). Required.
    --rpm <RPM>           Rolling-window requests-per-minute limit.
                          Default: 30 (conservative vs. NVIDIA's ~40 free tier).
    --host <HOST>         Listen address. Default: 127.0.0.1 (loopback only).
                          Use 0.0.0.0 only on a trusted network — the local
                          endpoint accepts any bearer token.
    --port <PORT>         Listen port. Default: 8000.
    --base-url <URL>      Upstream NIM base URL. Default:
                          https://integrate.api.nvidia.com
    --model <MODEL>       Default model id (e.g. z-ai/glm-5.2) injected when a
                          request omits 'model'. Without it, requests without
                          a model fail upstream exactly as if sent directly.
    --force-model <MODEL> Pin EVERY request to this model id, overriding what
                          each client asks for. Mutually exclusive with --model.
                          Model ids must match NVIDIA's catalog exactly
                          (curl $endpoint/v1/models to list them).
    -h, --help            Print this help and exit.
    -V, --version         Print the version and exit.

Model selection: by default the consumer (AI agent) names the model in each
request and the proxy passes it through untouched. --model fills a default
for requests that omit it; --force-model pins one model for everyone.

The proxy serializes upstream work: one NVIDIA request at a time, strict
FIFO, full response barrier (streaming included). No config file, no
dashboard, no Docker required — the invocation above is the whole setup.
";

/// Parse and validate a full `argv` (including, or excluding, argv[0] — the
/// parser skips any leading argument that is not a recognized flag).
pub fn parse<I, S>(argv: I) -> Parsed
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let raw: Vec<String> = argv.into_iter().map(Into::into).collect();
    let mut args = Args {
        nim_api_key: String::new(),
        rpm: DEFAULT_RPM,
        host: DEFAULT_HOST.to_owned(),
        port: DEFAULT_PORT,
        base_url: DEFAULT_BASE_URL.to_owned(),
        default_model: None,
        force_model: None,
    };
    let mut seen: Vec<&str> = Vec::new();

    let mut it = raw.into_iter().peekable();
    while let Some(a) = it.next() {
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (a, None),
        };
        match name.as_str() {
            "-h" | "--help" => return Parsed::Help(HELP.to_owned()),
            "-V" | "--version" => {
                return Parsed::Version(format!("nim-proxy {}\n", env!("CARGO_PKG_VERSION")))
            }
            "--nim-api-key" | "--rpm" | "--host" | "--port" | "--base-url" | "--model"
            | "--force-model" => {
                let value = match inline {
                    Some(v) => v,
                    None => match it.next() {
                        Some(v) => v,
                        None => {
                            return Parsed::Error(format!(
                                "error: {name} requires a value\n\n{HELP}"
                            ))
                        }
                    },
                };
                let dup = seen.contains(&name.as_str());
                seen.push(match name.as_str() {
                    "--nim-api-key" => "--nim-api-key",
                    "--rpm" => "--rpm",
                    "--host" => "--host",
                    "--port" => "--port",
                    "--model" => "--model",
                    "--force-model" => "--force-model",
                    _ => "--base-url",
                });
                if dup {
                    return Parsed::Error(format!("error: {name} given more than once\n\n{HELP}"));
                }
                if let Err(e) = assign(&mut args, &name, &value) {
                    return Parsed::Error(format!("error: {e}\n\n{HELP}"));
                }
            }
            other if other.starts_with('-') && !other.is_empty() && other != "-" => {
                return Parsed::Error(format!("error: unknown option '{other}'\n\n{HELP}"))
            }
            // Tolerate a stray argv[0]-style program name only in leading
            // position; anything else positional is a mistake worth hearing.
            other => {
                if seen.is_empty() && it.peek().is_some() {
                    continue; // argv[0]
                }
                if seen.is_empty() && it.peek().is_none() && !other.is_empty() {
                    // A single bare word: most likely someone passed the key
                    // positionally. Point them at the flag instead of the
                    // generic usage wall.
                    return Parsed::Error(format!(
                        "error: unexpected argument '{other}' — pass the key as --nim-api-key\n\n{HELP}"
                    ));
                }
                return Parsed::Error(format!("error: unexpected argument '{other}'\n\n{HELP}"));
            }
        }
    }

    if args.nim_api_key.is_empty() {
        return Parsed::Error(format!("error: --nim-api-key is required\n\n{HELP}"));
    }
    if args.default_model.is_some() && args.force_model.is_some() {
        return Parsed::Error(format!(
            "error: --model and --force-model are mutually exclusive — \
             --model fills in a default, --force-model pins every request\n\n{HELP}"
        ));
    }
    Parsed::Run(args)
}

/// Apply one `--flag value` pair; returns a human-readable error on rejection.
fn assign(args: &mut Args, name: &str, value: &str) -> Result<(), String> {
    match name {
        "--nim-api-key" => {
            if value.trim().is_empty() {
                return Err("--nim-api-key must not be empty".into());
            }
            args.nim_api_key = value.to_owned();
            Ok(())
        }
        "--rpm" => {
            let rpm = value
                .parse::<u32>()
                .map_err(|_| format!("--rpm must be a whole number, got '{value}'"))?;
            if rpm == 0 {
                return Err("--rpm must be at least 1 (0 would block every request)".into());
            }
            if rpm > MAX_RPM {
                return Err(format!("--rpm must be at most {MAX_RPM}, got {rpm}"));
            }
            args.rpm = rpm;
            Ok(())
        }
        "--host" => {
            let host = value.trim();
            if host.is_empty() {
                return Err("--host must not be empty".into());
            }
            args.host = host.to_owned();
            Ok(())
        }
        "--port" => {
            let port = value
                .parse::<u16>()
                .map_err(|_| format!("--port must be a port number (1-65535), got '{value}'"))?;
            if port == 0 {
                return Err(
                    "--port must be between 1 and 65535 (0 cannot be printed as an endpoint)"
                        .into(),
                );
            }
            args.port = port;
            Ok(())
        }
        "--base-url" => {
            let url = value.trim().trim_end_matches('/');
            if url.is_empty() {
                return Err("--base-url must not be empty".into());
            }
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(format!(
                    "--base-url must start with http:// or https://, got '{value}'"
                ));
            }
            args.base_url = url.to_owned();
            Ok(())
        }
        "--model" => {
            let model = value.trim();
            if model.is_empty() {
                return Err("--model must not be empty".into());
            }
            args.default_model = Some(model.to_owned());
            Ok(())
        }
        "--force-model" => {
            let model = value.trim();
            if model.is_empty() {
                return Err("--force-model must not be empty".into());
            }
            args.force_model = Some(model.to_owned());
            Ok(())
        }
        _ => unreachable!("caller matched the flag set"),
    }
}

impl fmt::Display for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "host={} port={} rpm={} base_url={} model={}",
            self.host,
            self.port,
            self.rpm,
            self.base_url,
            self.model_policy()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Parsed {
        parse(args.iter().copied())
    }

    #[test]
    fn full_invocation_parses() {
        let Parsed::Run(a) = run(&[
            "--nim-api-key",
            "nvapi-test",
            "--rpm",
            "30",
            "--host",
            "127.0.0.1",
            "--port",
            "8000",
        ]) else {
            panic!("expected Run");
        };
        assert_eq!(a.nim_api_key, "nvapi-test");
        assert_eq!(a.rpm, 30);
        assert_eq!(a.host, "127.0.0.1");
        assert_eq!(a.port, 8000);
        assert_eq!(a.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn minimum_invocation_uses_defaults() {
        let Parsed::Run(a) = run(&["--nim-api-key", "nvapi-x"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.rpm, DEFAULT_RPM);
        assert_eq!(a.host, DEFAULT_HOST);
        assert_eq!(a.port, DEFAULT_PORT);
        assert_eq!(a.endpoint_url(), "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn inline_equals_form_is_accepted() {
        let Parsed::Run(a) = run(&["--nim-api-key=nvapi-eq", "--rpm=17", "--port=9123"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.nim_api_key, "nvapi-eq");
        assert_eq!(a.rpm, 17);
        assert_eq!(a.port, 9123);
        assert_eq!(a.endpoint_url(), "http://127.0.0.1:9123/v1");
    }

    #[test]
    fn host_zero_shows_localhost_in_endpoint_url() {
        let Parsed::Run(a) = run(&["--nim-api-key", "k", "--host", "0.0.0.0"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.host, "0.0.0.0");
        assert_eq!(a.endpoint_url(), "http://localhost:8000/v1");
    }

    #[test]
    fn missing_key_is_an_error() {
        match run(&["--rpm", "30", "--port", "8000"]) {
            Parsed::Error(msg) => {
                assert!(msg.contains("--nim-api-key is required"), "{msg}");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn empty_key_is_an_error() {
        match run(&["--nim-api-key", ""]) {
            Parsed::Error(msg) => assert!(msg.contains("must not be empty"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn zero_rpm_is_rejected() {
        match run(&["--nim-api-key", "k", "--rpm", "0"]) {
            Parsed::Error(msg) => assert!(msg.contains("--rpm must be at least 1"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn negative_and_garbage_rpm_are_rejected() {
        for bad in ["-1", "abc", "3.5", ""] {
            match run(&["--nim-api-key", "k", "--rpm", bad]) {
                Parsed::Error(msg) => assert!(msg.contains("--rpm"), "{msg}"),
                _ => panic!("expected Error for rpm '{bad}'"),
            }
        }
    }

    #[test]
    fn absurd_rpm_is_rejected() {
        match run(&["--nim-api-key", "k", "--rpm", "999999"]) {
            Parsed::Error(msg) => assert!(msg.contains("at most"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn invalid_ports_are_rejected() {
        for bad in ["0", "65536", "99999", "http", ""] {
            match run(&["--nim-api-key", "k", "--port", bad]) {
                Parsed::Error(msg) => assert!(msg.contains("--port"), "{msg}"),
                _ => panic!("expected Error for port '{bad}'"),
            }
        }
    }

    #[test]
    fn empty_host_is_rejected() {
        match run(&["--nim-api-key", "k", "--host", "  "]) {
            Parsed::Error(msg) => assert!(msg.contains("--host"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn base_url_must_be_http_and_trailing_slash_is_trimmed() {
        match run(&[
            "--nim-api-key",
            "k",
            "--base-url",
            "integrate.api.nvidia.com",
        ]) {
            Parsed::Error(msg) => assert!(msg.contains("http://"), "{msg}"),
            _ => panic!("expected Error"),
        }
        let Parsed::Run(a) = run(&["--nim-api-key", "k", "--base-url", "http://x:9/"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.base_url, "http://x:9");
    }

    #[test]
    fn unknown_flag_is_rejected() {
        match run(&["--nim-api-key", "k", "--max-wait", "5"]) {
            Parsed::Error(msg) => assert!(msg.contains("unknown option '--max-wait'"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn duplicate_flag_is_rejected() {
        match run(&["--rpm", "5", "--rpm", "6", "--nim-api-key", "k"]) {
            Parsed::Error(msg) => assert!(msg.contains("more than once"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn flag_without_value_is_rejected() {
        match run(&["--nim-api-key", "k", "--port"]) {
            Parsed::Error(msg) => assert!(msg.contains("requires a value"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn positional_key_is_redirected_to_the_flag() {
        match run(&["nvapi-whoops"]) {
            Parsed::Error(msg) => {
                assert!(msg.contains("--nim-api-key"), "{msg}");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn help_and_version_short_forms() {
        assert!(matches!(run(&["-h"]), Parsed::Help(_)));
        assert!(matches!(run(&["--help"]), Parsed::Help(_)));
        let Parsed::Version(v) = run(&["-V"]) else {
            panic!("expected Version");
        };
        assert!(v.starts_with("nim-proxy "), "{v}");
    }

    #[test]
    fn argv0_program_name_is_ignored() {
        let Parsed::Run(a) = run(&["nim-proxy", "--nim-api-key", "k"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.nim_api_key, "k");
    }

    #[test]
    fn help_mentions_every_documented_flag() {
        let Parsed::Help(h) = run(&["--help"]) else {
            panic!("expected Help");
        };
        for token in [
            "--nim-api-key",
            "--rpm",
            "--host",
            "--port",
            "--base-url",
            "--model",
            "--force-model",
            "--help",
            "--version",
        ] {
            assert!(h.contains(token), "help must mention {token}");
        }
    }

    #[test]
    fn model_flag_sets_default_model() {
        let Parsed::Run(a) = run(&["--nim-api-key", "k", "--model", "z-ai/glm-5.2"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.default_model.as_deref(), Some("z-ai/glm-5.2"));
        assert_eq!(a.force_model, None);
        assert_eq!(
            a.model_policy(),
            "z-ai/glm-5.2 (default — used when a request omits 'model')"
        );
    }

    #[test]
    fn force_model_flag_pins_the_model() {
        let Parsed::Run(a) = run(&[
            "--nim-api-key",
            "k",
            "--force-model",
            "meta/llama-3.3-70b-instruct",
        ]) else {
            panic!("expected Run");
        };
        assert_eq!(
            a.force_model.as_deref(),
            Some("meta/llama-3.3-70b-instruct")
        );
        assert_eq!(a.default_model, None);
        assert_eq!(
            a.model_policy(),
            "meta/llama-3.3-70b-instruct (pinned — every request is rewritten to this model)"
        );
    }

    #[test]
    fn no_model_flags_is_passthrough() {
        let Parsed::Run(a) = run(&["--nim-api-key", "k"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.default_model, None);
        assert_eq!(a.force_model, None);
        assert_eq!(
            a.model_policy(),
            "passthrough (each client request names its own model)"
        );
    }

    #[test]
    fn model_and_force_model_are_mutually_exclusive() {
        match run(&[
            "--nim-api-key",
            "k",
            "--model",
            "a/b",
            "--force-model",
            "c/d",
        ]) {
            Parsed::Error(msg) => {
                assert!(msg.contains("mutually exclusive"), "{msg}");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn empty_model_flags_are_rejected() {
        for flag in ["--model", "--force-model"] {
            match run(&["--nim-api-key", "k", flag, "   "]) {
                Parsed::Error(msg) => {
                    assert!(msg.contains("must not be empty"), "{msg}");
                }
                _ => panic!("expected Error for {flag}"),
            }
        }
    }

    #[test]
    fn model_flags_accept_inline_equals_form() {
        let Parsed::Run(a) = run(&["--nim-api-key=k", "--model=z-ai/glm-5.2"]) else {
            panic!("expected Run");
        };
        assert_eq!(a.default_model.as_deref(), Some("z-ai/glm-5.2"));
    }

    #[test]
    fn model_flag_without_value_is_rejected() {
        match run(&["--nim-api-key", "k", "--model"]) {
            Parsed::Error(msg) => assert!(msg.contains("requires a value"), "{msg}"),
            _ => panic!("expected Error"),
        }
    }
}
