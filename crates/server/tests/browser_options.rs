#[path = "../examples/browser/options.rs"]
mod options;

use std::{ffi::OsString, io};

#[test]
fn localhost_browser_example_admits_only_one_bounded_numeric_port() -> Result<(), io::Error> {
    assert_eq!(options::port(std::iter::empty())?, 3000);
    for (argument, expected) in [("0", 0), ("3000", 3000), ("65535", 65535)] {
        assert_eq!(options::port([OsString::from(argument)])?, expected);
    }
    for argument in [
        "",
        "-1",
        "65536",
        "localhost:3000",
        "0.0.0.0:3000",
        "+80",
        "3000\n",
        "000000",
    ] {
        assert!(
            options::port([OsString::from(argument)]).is_err(),
            "{argument}"
        );
    }
    assert!(options::port([OsString::from("3000"), OsString::from("3001")]).is_err());
    Ok(())
}
