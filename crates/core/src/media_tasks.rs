#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MediaTaskToken {
    owner: u64,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaTaskKind {
    Playback,
    Recording,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaTaskState {
    Queued,
    Active,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaTaskError {
    Capacity,
    Ended,
    Unknown,
    Order,
    State,
    Recording,
}

#[derive(Clone, Copy)]
struct Task {
    token: MediaTaskToken,
    kind: MediaTaskKind,
    state: MediaTaskState,
}

enum Admission {
    Open,
    Ended,
}

pub struct MediaTasks {
    owner: u64,
    sequence: u64,
    admission: Admission,
    tasks: [Option<Task>; 8],
}

impl MediaTasks {
    pub const fn new(owner: u64) -> Self {
        Self {
            owner,
            sequence: 0,
            admission: Admission::Open,
            tasks: [None; 8],
        }
    }

    pub fn reserve(&mut self, kind: MediaTaskKind) -> Result<MediaTaskToken, MediaTaskError> {
        if matches!(self.admission, Admission::Ended) {
            return Err(MediaTaskError::Ended);
        }
        if kind == MediaTaskKind::Recording {
            for slot in &self.tasks {
                if matches!(slot, Some(task) if task.kind == kind) {
                    return Err(MediaTaskError::Recording);
                }
            }
        }
        let mut available = None;
        for (index, slot) in self.tasks.iter().enumerate() {
            if slot.is_none() {
                available = Some(index);
                break;
            }
        }
        let index = available.ok_or(MediaTaskError::Capacity)?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(MediaTaskError::Capacity)?;
        let token = MediaTaskToken {
            owner: self.owner,
            sequence,
        };
        self.tasks[index] = Some(Task {
            token,
            kind,
            state: MediaTaskState::Queued,
        });
        self.sequence = sequence;
        Ok(token)
    }

    pub fn state(&self, token: MediaTaskToken) -> Option<MediaTaskState> {
        for slot in &self.tasks {
            match slot {
                Some(task) if task.token == token => return Some(task.state),
                Some(_) | None => {}
            }
        }
        None
    }

    pub fn next_playback(&self) -> Option<MediaTaskToken> {
        let mut next = None;
        for slot in &self.tasks {
            let task = match slot {
                Some(task) if task.kind == MediaTaskKind::Playback => task,
                Some(_) | None => continue,
            };
            match task.state {
                MediaTaskState::Active | MediaTaskState::Paused => return None,
                MediaTaskState::Queued => {
                    if next.is_none_or(|token| task.token < token) {
                        next = Some(task.token);
                    }
                }
            }
        }
        next
    }

    fn index(&self, token: MediaTaskToken) -> Result<usize, MediaTaskError> {
        if matches!(self.admission, Admission::Ended) {
            return Err(MediaTaskError::Ended);
        }
        for (index, slot) in self.tasks.iter().enumerate() {
            if matches!(slot, Some(task) if task.token == token) {
                return Ok(index);
            }
        }
        Err(MediaTaskError::Unknown)
    }

    pub fn start(&mut self, token: MediaTaskToken) -> Result<(), MediaTaskError> {
        let index = self.index(token)?;
        let task = self.tasks[index].as_ref().ok_or(MediaTaskError::Unknown)?;
        if task.state == MediaTaskState::Queued
            && task.kind == MediaTaskKind::Playback
            && self.next_playback() != Some(token)
        {
            return Err(MediaTaskError::Order);
        }
        let task = self.tasks[index].as_mut().ok_or(MediaTaskError::Unknown)?;
        match task.state {
            MediaTaskState::Queued => {
                task.state = MediaTaskState::Active;
                Ok(())
            }
            MediaTaskState::Active => Ok(()),
            MediaTaskState::Paused => Err(MediaTaskError::State),
        }
    }

    pub fn pause(&mut self, token: MediaTaskToken) -> Result<(), MediaTaskError> {
        let index = self.index(token)?;
        let task = self.tasks[index].as_mut().ok_or(MediaTaskError::Unknown)?;
        match task.state {
            MediaTaskState::Queued => Err(MediaTaskError::State),
            MediaTaskState::Active | MediaTaskState::Paused => {
                task.state = MediaTaskState::Paused;
                Ok(())
            }
        }
    }

    pub fn resume(&mut self, token: MediaTaskToken) -> Result<(), MediaTaskError> {
        let index = self.index(token)?;
        let task = self.tasks[index].as_mut().ok_or(MediaTaskError::Unknown)?;
        match task.state {
            MediaTaskState::Queued => Err(MediaTaskError::State),
            MediaTaskState::Active | MediaTaskState::Paused => {
                task.state = MediaTaskState::Active;
                Ok(())
            }
        }
    }

    pub fn finish(&mut self, token: MediaTaskToken) -> bool {
        for slot in &mut self.tasks {
            if matches!(slot, Some(task) if task.token == token) {
                *slot = None;
                return true;
            }
        }
        false
    }

    pub fn cancel(&mut self) -> [Option<MediaTaskToken>; 8] {
        self.admission = Admission::Ended;
        self.tasks
            .each_mut()
            .map(|slot| slot.take().map(|task| task.token))
    }
}
