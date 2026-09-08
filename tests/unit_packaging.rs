//! Unit tests for the packaging authoring that no other test can reach.
//!
//! `packaging/windows/Package.wxs` used to be read by exactly one thing — WiX, on
//! a Windows runner, during a release build — so a mistake in it was found after a
//! tag was pushed, when the fix costs a version number rather than a commit. It
//! cost two: v0.16.0 to an illegal comment and v0.16.1 to a shortcut naming an
//! icon that was never declared (features/packaging-and-release.md phase 4).
//!
//! Two things read it now. CI's Windows leg *links* it on every commit
//! (`msi.ps1 -LinkOnly`), which is the only check that resolves a reference and so
//! the only one that can catch the second kind. These tests read it as text on
//! **any** platform, which catches less but catches it here, on the machine the
//! edit was made on, in the `cargo test` that was going to run anyway.

use std::path::PathBuf;

fn packaging_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every value of an attribute written `<needle>value"`, with the line it is on.
fn attribute_values(source: &str, needle: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = source[offset..].find(needle) {
        let value_start = offset + start + needle.len();
        let end = source[value_start..]
            .find('"')
            .unwrap_or_else(|| panic!("unterminated attribute value at byte {value_start}"));
        let line = source[..value_start].lines().count();
        found.push((line, source[value_start..value_start + end].to_string()));
        offset = value_start + end;
    }
    found
}

/// The text of every `<Icon .../>` element, with the line each starts on.
fn icon_elements(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = source[offset..].find("<Icon ") {
        let element_start = offset + start;
        let end = source[element_start..]
            .find('>')
            .unwrap_or_else(|| panic!("unterminated <Icon> element at byte {element_start}"));
        let line = source[..element_start].lines().count();
        found.push((line, source[element_start..element_start + end].to_string()));
        offset = element_start + end;
    }
    found
}

/// The one value of `attribute` on `element`, which every `<Icon>` here has.
fn attribute(element: &str, attribute: &str, line: usize) -> String {
    let needle = format!("{attribute}=\"");
    let values = attribute_values(element, &needle);
    assert_eq!(
        values.len(),
        1,
        "packaging/windows/Package.wxs:{line}: expected exactly one {attribute} on {element}"
    );
    values[0].1.clone()
}

/// XML forbids `--` inside a comment, and a comment may not end on `-`. WiX
/// rejects the file wholesale for it (`error WIX0104`), so a command-line flag
/// written into a comment the obvious way — with its two leading hyphens —
/// breaks the installer build and nothing else.
#[test]
fn the_wix_authoring_has_no_illegal_comment() {
    let source = packaging_file("packaging/windows/Package.wxs");

    let mut rest = source.as_str();
    let mut offset = 0usize;
    while let Some(start) = rest.find("<!--") {
        let after_open = start + "<!--".len();
        let body_start = offset + after_open;
        let tail = &rest[after_open..];
        let end = tail
            .find("-->")
            .unwrap_or_else(|| panic!("unterminated XML comment at byte {body_start}"));
        let body = &tail[..end];

        let line = source[..body_start].lines().count();
        assert!(
            !body.contains("--"),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot contain `--`, \
             and WiX refuses the whole file for it: {}",
            body.trim()
        );
        assert!(
            !body.ends_with('-'),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot end on `-`: {}",
            body.trim()
        );

        offset = body_start + end + "-->".len();
        rest = &source[offset..];
    }
}

/// An `Icon=` on a shortcut and the `ARPPRODUCTICON` property both name a row of
/// the Icon table by the `Id` of an `<Icon>` element. A name with no element
/// behind it is not caught when the file is parsed — it is caught by the linker,
/// at the end of the build (`error WIX0094`), which on this project means after
/// the tag is pushed and the other three platforms have already built.
#[test]
fn every_icon_reference_names_a_declared_icon() {
    let source = packaging_file("packaging/windows/Package.wxs");

    let declared: Vec<String> = icon_elements(&source)
        .into_iter()
        .map(|(line, element)| attribute(&element, "Id", line))
        .collect();
    assert!(!declared.is_empty(), "no <Icon> element to reference");

    // `<Icon Id="` is the declaration and does not contain `Icon="`, so this
    // finds the references and only those.
    let mut references = attribute_values(&source, "Icon=\"");
    references.extend(attribute_values(&source, "Id=\"ARPPRODUCTICON\" Value=\""));

    for (line, name) in references {
        assert!(
            declared.contains(&name),
            "packaging/windows/Package.wxs:{line}: `{name}` is not the Id of any <Icon> \
             element (declared: {declared:?}); WiX fails the link with error WIX0094"
        );
    }
}

/// The extension of what an `<Icon>` installs. The `SourceFile` here is a
/// preprocessor variable, so the answer lives in msi.ps1, which defines each one
/// with `-d "Name=<path>"` — the only place either file states a real path.
fn source_extension(file: &str, script: &str, line: usize) -> String {
    let path = match file.strip_prefix("$(").and_then(|v| v.strip_suffix(')')) {
        Some(variable) => {
            let defined = attribute_values(script, &format!("-d \"{variable}="));
            assert_eq!(
                defined.len(),
                1,
                "packaging/windows/Package.wxs:{line}: msi.ps1 does not define {variable} once"
            );
            defined[0].1.clone()
        }
        None => file.to_string(),
    };
    path.rsplit('.')
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}

/// Windows Installer decides how to read an icon from the *extension of its Id*,
/// not from the file it came from. An `.ico` source under an Id ending in
/// anything else installs and shows nothing, and says nothing about why.
#[test]
fn each_icon_id_carries_the_extension_of_its_source() {
    let source = packaging_file("packaging/windows/Package.wxs");
    let script = packaging_file("packaging/windows/msi.ps1");

    for (line, element) in icon_elements(&source) {
        let id = attribute(&element, "Id", line);
        let file = attribute(&element, "SourceFile", line);
        let extension = source_extension(&file, &script, line);
        assert!(
            id.to_lowercase().ends_with(&format!(".{extension}")),
            "packaging/windows/Package.wxs:{line}: the Icon Id `{id}` must end in \
             `.{extension}`, the extension of its source `{file}`, or the shell renders nothing"
        );
    }
}

/// The MSI and the code must agree on the service, in three places.
///
/// `features/windows-elevated-helper.md`: the installer registers the service, the
/// application pings it, and the verifier asserts it was installed and removed.
/// Nothing links those three but a string, and the failure modes are quiet ones —
/// a renamed service leaves the previous one registered on every upgraded machine,
/// and a lost argument starts a GUI with no window station that sits there doing
/// nothing. Read off Windows, like every other check in this file, because the
/// linker is the only other thing that would notice and it runs too late.
#[test]
fn the_installer_and_the_code_agree_about_the_helper_service() {
    let wxs = packaging_file("packaging/windows/Package.wxs");
    let verifier = packaging_file("packaging/windows/verify-msi.ps1");

    let name = yk_dist_manager::device::helper::SERVICE_NAME;
    let arg = yk_dist_manager::device::helper::SERVICE_ARG;

    assert!(
        wxs.contains(&format!("Name=\"{name}\"")),
        "Package.wxs does not register a service called `{name}`"
    );
    assert!(
        wxs.contains(&format!("Arguments=\"{arg}\"")),
        "Package.wxs does not start the service with `{arg}`"
    );
    assert!(
        verifier.contains(&format!("'{name}'")),
        "verify-msi.ps1 does not check for a service called `{name}`"
    );
    assert!(
        verifier.contains(arg),
        "verify-msi.ps1 does not check the service command line for `{arg}`"
    );

    // Both halves of the lifecycle: registered on install, and gone on uninstall.
    // The second is the one worth pinning — a LocalSystem service surviving an
    // uninstall is the worst outcome this feature can produce.
    assert!(
        wxs.contains("Remove=\"uninstall\""),
        "Package.wxs does not remove the service on uninstall"
    );
    assert!(
        wxs.contains("Stop=\"both\""),
        "Package.wxs does not stop the service on upgrade, so an upgrade would leave the previous \
         build's service running against the new protocol"
    );
}

/// Every code line of a PowerShell script, paired with its 1-based line number,
/// with `<# ... #>` blocks and whole-line `#` comments removed. Both kinds of
/// comment in these scripts *quote* the command lines they are explaining, so a
/// check that read the file raw would find every pattern it forbids in the prose
/// warning against it.
fn powershell_code(script: &str) -> Vec<(usize, String)> {
    let mut code = Vec::new();
    let mut in_block = false;
    for (index, line) in script.lines().enumerate() {
        let trimmed = line.trim();
        if in_block {
            if trimmed.contains("#>") {
                in_block = false;
            }
            continue;
        }
        if trimmed.starts_with("<#") {
            in_block = !trimmed.contains("#>");
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        code.push((index + 1, line.to_string()));
    }
    code
}

/// The command token of each `&` call on a line: what comes after `& `, up to the
/// next space. `& wix.exe --version` yields `wix.exe`; `& $exe --version` yields
/// `$exe`.
fn call_targets(line: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("& ") {
        let after = rest[at + 2..].trim_start();
        targets.push(after.split_whitespace().next().unwrap_or(""));
        rest = &rest[at + 2..];
    }
    targets
}

/// The application binary is linked into the Windows subsystem in release builds
/// (`src/main.rs`), so that no console flashes before the egui window appears.
/// PowerShell treats such an image differently from a console program: it starts
/// it, does not wait for it, and does not set `$LASTEXITCODE`. So the obvious
/// `$reported = & $exe --version` captures nothing, closes the pipe under a child
/// that is still writing to it, and then dies under `Set-StrictMode` on a
/// `$LASTEXITCODE` that was never set.
///
/// That is not a hypothesis: it is how the Windows leg of releases v0.18.3 and
/// v0.19.0 died, both at "Build the installer", both after the tag existed. CI's
/// per-commit check is `msi.ps1 -LinkOnly`, which packages a placeholder and so
/// never asks the binary anything — which is exactly why the cost was two version
/// numbers and why the guard belongs here, in a test that reads the scripts as
/// text on any platform.
///
/// Console programs — `dotnet`, `wix`, `signtool` — are invoked by name and are
/// not affected; the rule is only about calling a *variable* holding the
/// application's own path. `packaging/windows/gui-exe.ps1` is how that is done.
#[test]
fn nothing_asks_the_windows_binary_anything_without_waiting_for_it() {
    // The switches only the application answers. `wix.exe --version` is a console
    // program invoked by name and is left alone by the rule below.
    const APPLICATION_SWITCHES: [&str; 3] = ["--version", "--diagnose", "--help"];

    for script in ["msi.ps1", "verify-msi.ps1", "gui-exe.ps1"] {
        let relative = format!("packaging/windows/{script}");
        let source = packaging_file(&relative);

        for (line, text) in powershell_code(&source) {
            let switch = APPLICATION_SWITCHES
                .iter()
                .find(|switch| text.contains(**switch));
            let Some(switch) = switch else { continue };

            for target in call_targets(&text) {
                assert!(
                    !target.starts_with('$'),
                    "{relative}:{line}: `& {target} {switch}` runs the application binary \
                     without waiting for it — PowerShell does not wait for a Windows-subsystem \
                     image and leaves $LASTEXITCODE unset, which is how releases v0.18.3 and \
                     v0.19.0 died. Use Invoke-GuiExe from packaging/windows/gui-exe.ps1."
                );
            }
        }
    }

    // And the two scripts that do interrogate the binary reach it that way.
    for script in ["msi.ps1", "verify-msi.ps1"] {
        let relative = format!("packaging/windows/{script}");
        let source = packaging_file(&relative);
        assert!(
            source.contains("gui-exe.ps1"),
            "{relative} does not dot-source packaging/windows/gui-exe.ps1"
        );
        assert!(
            source.contains("Invoke-GuiExe"),
            "{relative} does not ask the binary about itself through Invoke-GuiExe"
        );
    }
}
