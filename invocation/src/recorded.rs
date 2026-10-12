/// What a reader took from an output an invocation writes as it runs, by
/// whether the output ran to its end. An invocation interrupted, or still
/// running, leaves the file cut off, and its reader keeps what lies ahead of
/// the cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded<T> {
    Complete(T),
    CutOff(T),
}

impl<T> Recorded<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Recorded<U> {
        match self {
            Recorded::Complete(t) => Recorded::Complete(f(t)),
            Recorded::CutOff(t) => Recorded::CutOff(f(t)),
        }
    }

    pub fn as_ref(&self) -> Recorded<&T> {
        match self {
            Recorded::Complete(t) => Recorded::Complete(t),
            Recorded::CutOff(t) => Recorded::CutOff(t),
        }
    }

    pub fn get(&self) -> &T {
        match self {
            Recorded::Complete(t) | Recorded::CutOff(t) => t,
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        match self {
            Recorded::Complete(t) | Recorded::CutOff(t) => t,
        }
    }

    pub fn into_inner(self) -> T {
        match self {
            Recorded::Complete(t) | Recorded::CutOff(t) => t,
        }
    }

    pub fn is_cut_off(&self) -> bool {
        matches!(self, Recorded::CutOff(_))
    }
}
