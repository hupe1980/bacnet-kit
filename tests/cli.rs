#![cfg(feature = "cli")]
use std::process::Command;
#[test]
fn local_demo_and_help() {
    // 001-commissioning/SR-004
    let exe = env!("CARGO_BIN_EXE_bacnet-kit");
    let demo = Command::new(exe).arg("demo").output().unwrap();
    assert!(demo.status.success());
    let out = String::from_utf8(demo.stdout).unwrap();
    assert!(out.contains("21.5 °C") && out.contains("normal"));
    let help = Command::new(exe).arg("--help").output().unwrap();
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("127.0.0.1:47808")
    );
    assert!(
        !Command::new(exe)
            .args(["read", "bad", "1"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn supervised_commands_stay_local_and_show_effective_readback() {
    // 002-supervised-commands/SR-003
    let exe = env!("CARGO_BIN_EXE_bacnet-kit");
    let run = Command::new(exe).arg("command-demo").output().unwrap();
    assert!(run.status.success());
    let text = String::from_utf8(run.stdout).unwrap();
    let rows: Vec<_> = text
        .lines()
        .skip(2)
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .collect();
    assert_eq!(rows[2], ["16", "Set", "18", "°C", "22", "°C"]);
    assert_eq!(rows[4], ["16", "Relinquish", "NULL", "20", "°C"]);
    // 011-typed-cli/SR-003
    let denied = Command::new(exe)
        .args(["serve", "--commands", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(
        String::from_utf8(denied.stderr)
            .unwrap()
            .contains("loopback")
    );
}

#[test]
fn dropped_udp_ack_still_reports_slot_and_effective_value() {
    // 002-supervised-commands/SR-004
    let out = Command::new(env!("CARGO_BIN_EXE_bacnet-kit"))
        .args(["command-demo", "--lose-ack"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert!(
        lines[4]
            .split_whitespace()
            .eq(["16", "Set", "18", "°C", "22", "°C"])
    );
    assert!(lines[5].contains("ACK missing"));
    assert!(lines.last().unwrap().ends_with("20 °C"));
}

#[test]
fn captures_both_fixture_modes_and_rejects_external_targets() {
    let exe = env!("CARGO_BIN_EXE_bacnet-kit");
    for (args, mode, service_bits) in [
        (vec!["pics"], "read-only", "[12, 14, 34]"),
        (
            vec!["pics", "--commands"],
            "supervised commands",
            "[12, 14, 15, 34]",
        ),
    ] {
        let result = Command::new(exe).args(&args).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report = String::from_utf8(result.stdout).unwrap();
        assert!(report.contains(mode) && report.contains(service_bits));
        assert!(report.contains("50 service bits and 65 object-type bits"));
        assert_eq!(report.contains("PRIORITY_ARRAY"), args.len() == 2);
        assert_eq!(report.contains("DS-WP-B"), args.len() == 2);
        assert!(report.contains("Unknown_Property") && report.contains("Product declaration"));
        assert_eq!(
            Command::new(exe).args(&args).output().unwrap().stdout,
            report.as_bytes()
        );
    }
    let failure = Command::new(exe)
        .args(["pics", "192.0.2.1:47808"])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
}

// 010-mstp-frame-inspection/SR-002
#[test]
fn mstp_offline_inputs_have_one_metadata_only_result() {
    use std::io::Write;
    use std::process::Stdio;
    let exe = env!("CARGO_BIN_EXE_bacnet-kit");
    let captured = [0x55, 0xff, 0, 1, 2, 0, 0, 0x40];
    let binary = |input: &[u8]| {
        let mut child = Command::new(exe)
            .args(["mstp-frame", "--raw"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    };
    let raw = binary(&captured);
    assert!(raw.status.success());
    assert_eq!(
        String::from_utf8_lossy(&raw.stdout),
        "Frame        TOKEN (0)\nSource       2\nDestination  1\nData octets  0\nWire octets  8\nPad          none\n"
    );
    for hex in ["55ff000102000040", "55 FF 00 01\n02 00 00 40"] {
        let text = Command::new(exe)
            .args(["mstp-frame", hex])
            .output()
            .unwrap();
        assert!(text.status.success());
        assert_eq!(raw.stdout, text.stdout);
    }
    for input in ["55 f", "secret-content", "55ff000102000040ff00"] {
        let out = Command::new(exe)
            .args(["mstp-frame", input])
            .output()
            .unwrap();
        assert!(!out.status.success() && out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains(input));
    }
    let over = binary(&vec![0; bacnet_kit::mstp::MAX_RECORD + 1]);
    assert!(!over.status.success() && over.stdout.is_empty());
    assert!(String::from_utf8_lossy(&over.stderr).contains("1517"));
}

#[test]
fn installed_commands_have_offline_help_and_version() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bacnet-kit"))
            .args(args)
            .output()
            .unwrap()
    };
    let root = run(&[]);
    assert!(root.status.success() && root.stderr.is_empty());
    assert_eq!(root.stdout, run(&["--help"]).stdout);
    let version = run(&["--version"]);
    assert!(version.status.success() && version.stderr.is_empty());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .contains(env!("CARGO_PKG_VERSION"))
    );
    for command in [
        "demo",
        "batch-demo",
        "command-demo",
        "pics",
        "serve",
        "read",
        "inspect",
        "batch",
        "foreign",
        "watch",
        "mstp-frame",
        "sc-check",
        "sc-read",
    ] {
        let out = run(&[command, "--help"]);
        let installed = match command {
            "sc-read" => cfg!(feature = "sc-node"),
            "sc-check" => cfg!(feature = "sc-pki"),
            _ => true,
        };
        if installed {
            assert!(out.status.success() && out.stderr.is_empty(), "{command}");
            assert!(String::from_utf8_lossy(&out.stdout).contains("Usage:"));
            assert_eq!(out.stdout, run(&["help", command]).stdout);
        } else {
            assert_eq!(out.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&out.stderr).contains("cargo build --features"));
        }
    }
}

// 011-typed-cli/SR-001
#[test]
fn invalid_invocations_exit_before_operations() {
    let mut excessive = vec!["batch", "127.0.0.1:0"];
    excessive.extend(["analog-input:1/units"; 20]);
    excessive.extend(["--device", "123"]);
    excessive.extend(["analog-input:1/units"; 13]);
    for args in [
        excessive,
        vec!["batch", "127.0.0.1:0"],
        vec!["batch", "127.0.0.1:0", "bad-ref"],
        vec!["watch", "--seconds", "0", "127.0.0.1:0", "123", "1"],
        vec!["foreign", "--ttl", "65536", "127.0.0.1:0"],
        vec!["inspect", "127.0.0.1:0", "--device", "4194303"],
        vec!["read", "bad-route@127.0.0.1:0", "1"],
        vec!["pics", "--commands", "--commands"],
        vec!["mstp-frame", "--raw", "55ff"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_bacnet-kit"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty() && !out.stderr.is_empty());
    }
}

// 011-typed-cli/SR-002
#[test]
fn protected_commands_omit_values_from_parse_errors() {
    for command in ["mstp-frame", "sc-read", "sc-check"] {
        let out = Command::new(env!("CARGO_BIN_EXE_bacnet-kit"))
            .args([command, "--private-marker-name"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("private-marker-name"));
    }
}
