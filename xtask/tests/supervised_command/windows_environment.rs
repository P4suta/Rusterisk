#![cfg(windows)]

use super::checked;
use std::process::Command;
use xtask::process::SupervisedCommand;

#[derive(Clone, Copy, Debug)]
enum Change {
    Set(&'static str, &'static str),
    Remove(&'static str),
    Clear,
}

fn compare(changes: &[Change]) -> Result<(), String> {
    let mut request = checked(SupervisedCommand::new("unused-program"))?;
    let mut reference = Command::new("unused-program");
    for &change in changes {
        match change {
            Change::Set(name, value) => {
                checked(request.env(name, value))?;
                reference.env(name, value);
            }
            Change::Remove(name) => {
                checked(request.env_remove(name))?;
                reference.env_remove(name);
            }
            Change::Clear => {
                checked(request.env_clear())?;
                reference.env_clear();
            }
        }
    }
    let actual = request.into_std();
    assert_eq!(
        actual.get_envs().collect::<Vec<_>>(),
        reference.get_envs().collect::<Vec<_>>(),
        "{changes:?}"
    );
    Ok(())
}

#[test]
fn windows_environment_aliases_preserve_standard_call_order_and_key_spelling() -> Result<(), String>
{
    use Change::{Clear, Remove, Set};
    const CASES: [&[Change]; 9] = [
        &[Set("Path", "first"), Set("PATH", "latest")],
        &[Set("PATH", "first"), Set("Path", "latest")],
        &[Set("Path", "first"), Remove("PATH")],
        &[Remove("Path"), Set("PATH", "latest")],
        &[Remove("PATH"), Set("Path", "latest")],
        &[Clear, Set("Path", "first"), Set("PATH", "latest")],
        &[Clear, Set("Path", "first"), Remove("PATH")],
        &[Clear, Remove("Path"), Set("PATH", "latest")],
        &[Set("Path", "before-clear"), Clear, Set("PATH", "latest")],
    ];
    for changes in CASES {
        compare(changes)?;
    }
    Ok(())
}
