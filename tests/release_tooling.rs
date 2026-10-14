// The Windows release surface: the four scripts that are shipped inside the
// published archive, and the properties `RELEASING.md` and `.gitattributes`
// promise about them.
//
// `RELEASING.md` (in the releases repository) lists exactly what a release
// archive must carry - the executable, `setup.cmd`, `setup.ps1`, `uninstall.cmd`,
// `uninstall.ps1`, and the licence text - and `.gitattributes` pins the scripts
// to CRLF because "a batch file that arrives with LF endings is a lottery".
// Nothing checked either of those, so a lost `.gitattributes` entry, an editor
// that normalised the endings, or a script renamed without its sibling would
// have gone unnoticed until an installer misbehaved on a user's machine.
//
// These run without Windows-specific APIs, so they hold on any platform.

use std::fs;
use std::path::{Path, PathBuf};

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

const SCRIPTS: [&str; 4] = [
    "tools/setup.cmd",
    "tools/setup.ps1",
    "tools/uninstall.cmd",
    "tools/uninstall.ps1",
];

#[test]
fn the_release_scripts_are_present() {
    for rel in SCRIPTS {
        let p = repo(rel);
        assert!(p.is_file(), "{rel} is missing from the release payload");
        assert!(
            fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > 0,
            "{rel} is empty"
        );
    }
}

#[test]
fn the_batch_and_powershell_scripts_are_crlf() {
    // The uninstaller deletes itself while cmd is still reading it, and cmd
    // reads a batch file block by block - which is exactly where LF endings are a
    // lottery. `.gitattributes` pins `*.cmd` and `*.ps1` to CRLF; this is the
    // check that the pin actually reached the file.
    for rel in SCRIPTS {
        let bytes = fs::read(repo(rel)).unwrap();
        let lf = bytes.iter().filter(|b| **b == b'\n').count();
        let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
        assert!(crlf > 0, "{rel} has no CRLF line endings at all");
        assert_eq!(
            lf, crlf,
            "{rel} has {lf} newlines but only {crlf} CRLF pairs - it was normalised to LF"
        );
        // ...and nothing that would be mangled by cmd's parser.
        assert!(
            !bytes.contains(&b'\n') || !bytes.windows(2).any(|w| w[0] != b'\r' && w[1] == b'\n'),
            "{rel} contains a bare LF"
        );
    }
}

#[test]
fn each_cmd_shim_runs_the_ps1_beside_it() {
    // The shims find their PowerShell sibling through `%~dp0`, so the pair has
    // to travel together in the archive. A script copied out on its own fails at
    // the first line, which is the sort of thing only a user sees.
    for (cmd, ps1) in [
        ("tools/setup.cmd", "setup.ps1"),
        ("tools/uninstall.cmd", "uninstall.ps1"),
    ] {
        let text = fs::read_to_string(repo(cmd)).unwrap();
        assert!(
            text.contains(&format!("\"%~dp0{ps1}\"")),
            "{cmd} must invoke {ps1} through %~dp0 so the archive can be run from anywhere"
        );
    }
}

#[test]
fn the_uninstaller_still_clears_up_after_itself() {
    // The comment in uninstall.cmd explains the trick: `goto` with no label ends
    // the batch file, so cmd stops reading it and the rest of the line runs
    // outside the file - which is what makes deleting the file while it runs
    // possible. If that line is ever reflowed or reordered, the uninstaller
    // breaks in the way the comment warns about, and only on a real machine.
    let text = fs::read_to_string(repo("tools/uninstall.cmd")).unwrap();
    assert!(
        text.contains("(goto) 2>nul & del /q \"%~f0\""),
        "the self-delete line is gone or has been reordered"
    );
    let goto_line = text
        .lines()
        .position(|l| l.contains("(goto)"))
        .expect("no goto line");
    let exit_line = text
        .lines()
        .position(|l| l.trim_start().starts_with("exit /b"))
        .expect("no exit line");
    assert!(
        goto_line < exit_line,
        "the self-delete must happen before the script exits"
    );
}

#[test]
fn the_licence_files_the_archive_carries_are_present() {
    // RELEASING.md: "the licence terms must travel with the binary, so the
    // archive carries text, not links". LICENSE.txt and NOTICE.txt in the
    // archive are copies of these.
    for rel in ["LICENSE.md", "NOTICE.md"] {
        let p = repo(rel);
        assert!(p.is_file(), "{rel} is missing - the archive copies it in as text");
        assert!(fs::metadata(&p).unwrap().len() > 200, "{rel} looks truncated");
    }
}
