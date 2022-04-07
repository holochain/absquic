//! Helper type for a collection of Bytes

use std::collections::vec_deque;
use std::collections::VecDeque;
use std::io::IoSlice;

/// Helper type for a collection of Bytes
pub struct BufList<B: bytes::Buf + Sized>(VecDeque<B>);

impl<B: bytes::Buf + Sized> Default for BufList<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: bytes::Buf + Sized> BufList<B> {
    /// Construct a new BufList
    pub fn new() -> Self {
        Self(VecDeque::new())
    }

    /// Push a new item onto the BufList
    pub fn push_back(&mut self, b: B) {
        self.0.push_back(b);
    }

    /// Write represented bytes to a new `Vec<u8>`
    pub fn into_vec(mut self) -> Vec<u8> {
        use bytes::Buf;
        let mut out = Vec::with_capacity(self.remaining());
        while self.has_remaining() {
            let chunk = self.chunk();
            out.extend_from_slice(chunk);
            let len = chunk.len();
            self.advance(len);
        }
        out
    }
}

impl<B: bytes::Buf + Sized> bytes::Buf for BufList<B> {
    fn remaining(&self) -> usize {
        self.0.iter().map(|x| x.remaining()).sum()
    }

    fn chunk(&self) -> &[u8] {
        if let Some(front) = self.0.front() {
            front.chunk()
        } else {
            b""
        }
    }

    #[allow(clippy::comparison_chain)]
    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            if let Some(mut buf) = self.0.pop_front() {
                let buf_len = buf.remaining();
                if buf_len == cnt {
                    return;
                } else if buf_len < cnt {
                    cnt -= buf_len;
                    continue;
                } else {
                    buf.advance(cnt);
                    self.0.push_front(buf);
                    return;
                }
            } else {
                panic!("failed to advance, no more data");
            }
        }
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [IoSlice<'a>]) -> usize {
        let count = std::cmp::min(dst.len(), self.0.len());
        let mut iter = self.0.iter();
        for dst in dst.iter_mut().take(count) {
            *dst = IoSlice::new(iter.next().unwrap().chunk());
        }
        count
    }
}

/// IntoIter for BufList
pub struct BufListIntoIter<B: bytes::Buf + Sized>(vec_deque::IntoIter<B>);

impl<B: bytes::Buf + Sized> std::iter::Iterator for BufListIntoIter<B> {
    type Item = B;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

impl<B: bytes::Buf + Sized> std::iter::IntoIterator for BufList<B> {
    type Item = B;
    type IntoIter = BufListIntoIter<B>;

    fn into_iter(self) -> Self::IntoIter {
        BufListIntoIter(self.0.into_iter())
    }
}
