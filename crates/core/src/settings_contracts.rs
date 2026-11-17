//! Releasing a reference consumes its handle.
//!
//! ```compile_fail,E0382
//! use rusterisk_core::{SettingsError, SettingsLifetimes};
//!
//! let mut settings = SettingsLifetimes::new(1);
//! let reference = settings.retain(settings.current())?;
//! settings.release(reference)?;
//! settings.release(reference)?;
//! # Ok::<(), SettingsError>(())
//! ```
//!
//! References omit `Clone`.
//!
//! ```compile_fail,E0599
//! use rusterisk_core::{SettingsError, SettingsLifetimes};
//!
//! let mut settings = SettingsLifetimes::new(1);
//! let reference = settings.retain(settings.current())?;
//! let duplicate = reference.clone();
//! settings.release(reference)?;
//! settings.release(duplicate)?;
//! # Ok::<(), SettingsError>(())
//! ```
//!
//! A generation doesn't construct a reference.
//!
//! ```compile_fail
//! use rusterisk_core::{SettingsLifetimes, SettingsReference};
//!
//! let settings = SettingsLifetimes::new(1);
//! let generation = settings.current();
//! let reference = SettingsReference { generation };
//! ```
//!
//! Releasing settings requires a reference handle.
//!
//! ```compile_fail,E0308
//! use rusterisk_core::{SettingsError, SettingsLifetimes};
//!
//! let mut settings = SettingsLifetimes::new(1);
//! let generation = settings.current();
//! settings.release(generation)?;
//! # Ok::<(), SettingsError>(())
//! ```
//!
//! A retained reference exposes its generation before release.
//!
//! ```no_run
//! use rusterisk_core::{SettingsError, SettingsLifetimes, SettingsReference};
//!
//! let mut settings = SettingsLifetimes::new(1);
//! let generation = settings.current();
//! let reference: SettingsReference = settings.retain(generation)?;
//! assert_eq!(reference.generation(), generation);
//! settings.release(reference)?;
//! # Ok::<(), SettingsError>(())
//! ```
