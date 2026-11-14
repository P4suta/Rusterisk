#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtmfDigit {
    Zero,
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Star,
    Hash,
    A,
    B,
    C,
    D,
}

impl DtmfDigit {
    pub const fn from_event(event: u8) -> Option<Self> {
        match event {
            0 => Some(Self::Zero),
            1 => Some(Self::One),
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            4 => Some(Self::Four),
            5 => Some(Self::Five),
            6 => Some(Self::Six),
            7 => Some(Self::Seven),
            8 => Some(Self::Eight),
            9 => Some(Self::Nine),
            10 => Some(Self::Star),
            11 => Some(Self::Hash),
            12 => Some(Self::A),
            13 => Some(Self::B),
            14 => Some(Self::C),
            15 => Some(Self::D),
            _ => None,
        }
    }

    pub const fn from_symbol(symbol: char) -> Option<Self> {
        match symbol {
            '0' => Some(Self::Zero),
            '1' => Some(Self::One),
            '2' => Some(Self::Two),
            '3' => Some(Self::Three),
            '4' => Some(Self::Four),
            '5' => Some(Self::Five),
            '6' => Some(Self::Six),
            '7' => Some(Self::Seven),
            '8' => Some(Self::Eight),
            '9' => Some(Self::Nine),
            '*' => Some(Self::Star),
            '#' => Some(Self::Hash),
            'A' => Some(Self::A),
            'B' => Some(Self::B),
            'C' => Some(Self::C),
            'D' => Some(Self::D),
            _ => None,
        }
    }

    pub const fn event(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
            Self::Four => 4,
            Self::Five => 5,
            Self::Six => 6,
            Self::Seven => 7,
            Self::Eight => 8,
            Self::Nine => 9,
            Self::Star => 10,
            Self::Hash => 11,
            Self::A => 12,
            Self::B => 13,
            Self::C => 14,
            Self::D => 15,
        }
    }

    pub const fn symbol(self) -> char {
        match self {
            Self::Zero => '0',
            Self::One => '1',
            Self::Two => '2',
            Self::Three => '3',
            Self::Four => '4',
            Self::Five => '5',
            Self::Six => '6',
            Self::Seven => '7',
            Self::Eight => '8',
            Self::Nine => '9',
            Self::Star => '*',
            Self::Hash => '#',
            Self::A => 'A',
            Self::B => 'B',
            Self::C => 'C',
            Self::D => 'D',
        }
    }
}
