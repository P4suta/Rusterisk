use std::path::Path;
use xtask::browser::{patch_declaration, BrowserError, Declaration};

fn original(declaration: Declaration) -> Result<Vec<u8>, BrowserError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let bytes = std::fs::read(
        root.join("examples/browser/node_modules/sip.js")
            .join(declaration.path()),
    )?;
    let source = String::from_utf8(bytes).map_err(|_| BrowserError::Declaration)?;
    Ok(source
        .replace(declaration.patched(), declaration.original())
        .into_bytes())
}

#[test]
fn fixed_signaling_declarations_accept_only_the_bound_original_and_idempotent_correction(
) -> Result<(), BrowserError> {
    for declaration in Declaration::ALL {
        let original = original(declaration)?;
        let corrected = patch_declaration(declaration, "0.21.2", &original)?;
        assert_ne!(corrected, original);
        let source = std::str::from_utf8(&corrected).map_err(|_| BrowserError::Declaration)?;
        assert_eq!(source.matches(declaration.patched()).count(), 1);
        assert_eq!(source.matches(declaration.original()).count(), 0);
        assert_eq!(
            patch_declaration(declaration, "0.21.2", &corrected)?,
            corrected
        );
    }
    Ok(())
}

#[test]
fn declaration_corrections_reject_other_versions_words_fields_and_file_identity(
) -> Result<(), BrowserError> {
    for declaration in Declaration::ALL {
        let source =
            String::from_utf8(original(declaration)?).map_err(|_| BrowserError::Declaration)?;
        assert!(patch_declaration(declaration, "0.21.3", source.as_bytes()).is_err());
        for invalid in [
            source.replace("delegate?:", "handler?:"),
            source.replace("import {", "import type {"),
            format!("{source}{}", declaration.original()),
            source.replace("@public", "@internal"),
            String::new(),
        ] {
            assert_ne!(invalid, source);
            assert!(patch_declaration(declaration, "0.21.2", invalid.as_bytes()).is_err());
        }
        let other = match declaration {
            Declaration::Incoming => Declaration::Outgoing,
            Declaration::Outgoing | Declaration::Invite | Declaration::Subscribe => {
                Declaration::Incoming
            }
        };
        assert!(patch_declaration(other, "0.21.2", source.as_bytes()).is_err());
    }
    Ok(())
}
