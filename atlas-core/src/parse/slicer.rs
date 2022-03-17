use super::ast::{ByteIndex, ByteOffset, Span};

// we can't use iterators since we need to be able to peek
// without using mut

#[derive(Clone)]
pub struct StringSlicer<'input> {
    string: &'input str,
    idx: ByteIndex,
}

impl<'input> StringSlicer<'input> {
    pub fn new(input: &'input str) -> Self {
        StringSlicer {
            string: input,
            idx: ByteIndex(0),
        }
    }

    pub fn pos(&self) -> ByteIndex {
        self.idx
    }

    pub fn hit_end(&self) -> bool {
        self.idx.to_usize() >= self.string.len()
    }

    pub fn next(&mut self) -> Option<(ByteIndex, char, ByteIndex)> {
        match self.peek() {
            Some((start, ch, end)) => {
                self.idx = end;
                Some((start, ch, end))
            }
            None => None,
        }
    }

    pub fn peek(&self) -> Option<(ByteIndex, char, ByteIndex)> {
        if self.idx.to_usize() < self.string.len() {
            let s = &self.string[self.idx.to_usize()..];
            match s.chars().next() {
                Some(ch) => Some((self.idx, ch, self.idx + ByteOffset(ch.len_utf8() as i64))),
                None => None,
            }
        } else {
            None
        }
    }

    pub fn test_peek<F>(&self, mut test: F) -> bool
    where
        F: FnMut(char) -> bool,
    {
        self.peek().map_or(false, |(_, ch, _)| test(ch))
    }

    pub fn test_look<F>(&self, idx: usize, test: F) -> bool
    where
        F: FnMut(char) -> bool,
    {
        self.string[self.idx.to_usize()..]
            .chars()
            .nth(idx)
            .map_or(false, test)
    }

    pub fn take_while<F>(&mut self, mut test: F) -> (ByteIndex, ByteIndex)
    where
        F: FnMut(char) -> bool,
    {
        let start = self.pos();
        let mut end = self.pos();

        while let Some((_, ch, cend)) = self.peek() {
            if !test(ch) {
                break;
            }
            self.next();
            end = cend;
        }
        (start, end)
    }

    // extract a slice or a span

    // start and end are inclusive
    pub fn slice(&self, s: ByteIndex, e: ByteIndex) -> &'input str {
        &self.string[s.to_usize()..e.to_usize()]
    }

    pub fn span(&self, s: Span) -> &'input str {
        self.slice(s.start(), s.end())
    }
}

#[cfg(test)]
mod tests {
    use super::super::ast::ByteIndex;
    use super::StringSlicer;

    #[test]
    fn iterate() {
        let mut s = StringSlicer::new("Flub");
        assert_eq!(s.peek(), Some((ByteIndex(0), 'F', ByteIndex(1))));
        assert_eq!(s.next(), Some((ByteIndex(0), 'F', ByteIndex(1))));
        assert_eq!(s.peek(), Some((ByteIndex(1), 'l', ByteIndex(2))));
        assert_eq!(s.next(), Some((ByteIndex(1), 'l', ByteIndex(2))));
        s.next();
        s.next();
        assert_eq!(s.next(), None);
    }
}
