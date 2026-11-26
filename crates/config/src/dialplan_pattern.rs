use crate::Dialplan;
use std::collections::BTreeMap;
use std::{cmp::Ordering, iter::Peekable, str::Bytes};

#[derive(Clone, Copy)]
enum Atom {
    Set(u128),
    OneOrMore,
    ZeroOrMore,
    Invalid,
}

impl Atom {
    fn rank(self) -> (u32, u128) {
        match self {
            Self::Set(bits) => (bits.count_ones(), !bits.reverse_bits()),
            Self::OneOrMore => (129, 0),
            Self::ZeroOrMore => (130, 0),
            Self::Invalid => (131, 0),
        }
    }
}

fn bit(byte: u8) -> u128 {
    1u128.checked_shl(u32::from(byte)).unwrap_or(0)
}

fn range(first: u8, last: u8) -> u128 {
    (first..=last).fold(0, |bits, byte| bits | bit(byte))
}

fn class_byte(bytes: &mut Peekable<Bytes<'_>>) -> Option<u8> {
    match bytes.next()? {
        b'\\' => bytes.next(),
        b']' => None,
        byte => Some(byte),
    }
}

fn class(bytes: &mut Peekable<Bytes<'_>>) -> Option<u128> {
    let mut bits = 0;
    loop {
        let first = class_byte(bytes)?;
        let mut last = first;
        if bytes.next_if_eq(&b'-').is_some() {
            if bytes.peek() == Some(&b']') {
                bits |= bit(b'-');
            } else {
                last = class_byte(bytes)?;
            }
        }
        if last < first || !last.is_ascii() || !first.is_ascii() {
            return None;
        }
        bits |= range(first, last);
        if bytes.next_if_eq(&b']').is_some() {
            return Some(bits);
        }
    }
}

fn atoms(pattern: &str) -> impl Iterator<Item = Atom> + '_ {
    let mut bytes = pattern.bytes().peekable();
    std::iter::from_fn(move || {
        let byte = bytes.find(|byte| *byte != b'-')?;
        let atom = match byte {
            b'[' => class(&mut bytes).map_or(Atom::Invalid, Atom::Set),
            b'X' | b'x' => Atom::Set(range(b'0', b'9')),
            b'Z' | b'z' => Atom::Set(range(b'1', b'9')),
            b'N' | b'n' => Atom::Set(range(b'2', b'9')),
            b'.' => Atom::OneOrMore,
            b'!' => Atom::ZeroOrMore,
            byte if byte.is_ascii() => Atom::Set(bit(byte)),
            _ => Atom::Invalid,
        };
        Some(atom)
    })
}

pub(super) fn valid(extension: &str) -> bool {
    extension.strip_prefix('_').is_none_or(|pattern| {
        let mut tokens = atoms(pattern).peekable();
        tokens.peek().is_some() && tokens.all(|atom| !matches!(atom, Atom::Invalid))
    })
}

fn ranks(pattern: &str) -> impl Iterator<Item = (u32, u128)> + '_ {
    atoms(pattern)
        .map(Atom::rank)
        .chain(std::iter::once((u32::MAX, 0)))
}

fn matches(pattern: &str, extension: &str) -> bool {
    let mut dialed = extension.bytes().filter(|byte| *byte != b'-');
    for atom in atoms(pattern) {
        match atom {
            Atom::Set(bits) if dialed.next().is_some_and(|byte| bits & bit(byte) != 0) => {}
            Atom::OneOrMore => return dialed.next().is_some(),
            Atom::ZeroOrMore => return true,
            Atom::Set(_) | Atom::Invalid => return false,
        }
    }
    dialed.next().is_none()
}

impl Dialplan {
    pub fn compile(self) -> CompiledDialplan {
        let mut contexts = BTreeMap::<String, BTreeMap<u16, ExtensionRoutes>>::new();
        for (index, step) in self.steps.iter().enumerate() {
            let routes = contexts
                .entry(step.context.clone())
                .or_default()
                .entry(step.priority)
                .or_default();
            if step.extension.starts_with('_') {
                routes.patterns.push(index);
            } else {
                routes.exact.push(index);
            }
        }
        for priorities in contexts.values_mut() {
            for routes in priorities.values_mut() {
                routes.exact.sort_by(|&left, &right| {
                    digits(&self.steps[left].extension).cmp(digits(&self.steps[right].extension))
                });
                routes.patterns.sort_by(|&left, &right| {
                    ranks(&self.steps[left].extension[1..])
                        .cmp(ranks(&self.steps[right].extension[1..]))
                });
            }
        }
        CompiledDialplan {
            program: self,
            contexts,
        }
    }

    pub fn position(&self, context: &str, extension: &str, priority: u16) -> Option<u16> {
        let mut best: Option<(usize, &str)> = None;
        for (index, step) in self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.context == context && step.priority == priority)
        {
            if let Some(pattern) = step.extension.strip_prefix('_') {
                if matches(pattern, extension)
                    && best.is_none_or(|(_, previous)| {
                        ranks(pattern).cmp(ranks(previous)) == Ordering::Less
                    })
                {
                    best = Some((index, pattern));
                }
            } else if step
                .extension
                .bytes()
                .filter(|byte| *byte != b'-')
                .eq(extension.bytes().filter(|byte| *byte != b'-'))
            {
                return u16::try_from(index).ok();
            }
        }
        best.and_then(|(index, _)| u16::try_from(index).ok())
    }
}

fn digits(extension: &str) -> impl Iterator<Item = u8> + '_ {
    extension.bytes().filter(|byte| *byte != b'-')
}

#[derive(Clone, Debug, Default)]
struct ExtensionRoutes {
    exact: Vec<usize>,
    patterns: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct CompiledDialplan {
    program: Dialplan,
    contexts: BTreeMap<String, BTreeMap<u16, ExtensionRoutes>>,
}

impl CompiledDialplan {
    pub fn program(&self) -> &Dialplan {
        &self.program
    }

    pub fn position(&self, context: &str, extension: &str, priority: u16) -> Option<u16> {
        let routes = self.contexts.get(context)?.get(&priority)?;
        let steps = &self.program.steps;
        let first = routes.exact.partition_point(|&index| {
            digits(&steps[index].extension)
                .cmp(digits(extension))
                .is_lt()
        });
        let exact = routes
            .exact
            .get(first)
            .filter(|&&index| digits(&steps[index].extension).eq(digits(extension)));
        let selected = exact.or_else(|| {
            routes
                .patterns
                .iter()
                .find(|&&index| matches(&steps[index].extension[1..], extension))
        })?;
        u16::try_from(*selected).ok()
    }
}
