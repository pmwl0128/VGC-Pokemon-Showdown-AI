//! A fixed-capacity vector on the stack, for the short lists rule code builds (handlers,
//! targets, switchers) without touching the heap. Pushing past the capacity panics.

use std::mem::MaybeUninit;

pub struct FixedVec<T: Copy, const N: usize> {
    len: usize,
    buf: [MaybeUninit<T>; N],
}

impl<T: Copy, const N: usize> FixedVec<T, N> {
    #[inline]
    pub fn new() -> Self {
        FixedVec { len: 0, buf: [const { MaybeUninit::uninit() }; N] }
    }

    #[inline]
    pub fn push(&mut self, x: T) {
        assert!(self.len < N, "FixedVec: capacity {N} exceeded");
        self.buf[self.len].write(x);
        self.len += 1;
    }
}

impl<T: Copy, const N: usize> Default for FixedVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy, const N: usize> std::ops::Deref for FixedVec<T, N> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        // SAFETY: entries below `len` were written by `push`.
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const T, self.len) }
    }
}

impl<T: Copy, const N: usize> std::ops::DerefMut for FixedVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: as above.
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut T, self.len) }
    }
}

impl<T: Copy, const N: usize> FromIterator<T> for FixedVec<T, N> {
    #[inline]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = FixedVec::new();
        for x in iter {
            v.push(x);
        }
        v
    }
}

impl<'a, T: Copy, const N: usize> IntoIterator for &'a FixedVec<T, N> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::FixedVec;

    #[test]
    fn push_and_slice() {
        let mut v: FixedVec<u8, 3> = FixedVec::new();
        assert!(v.is_empty());
        v.push(3);
        v.push(1);
        v.sort();
        assert_eq!(&v[..], &[1, 3]);
        let w: FixedVec<u8, 4> = (0..4).collect();
        assert_eq!(w.len(), 4);
    }
}
