//! Defines the Row type which makes up lines in the grid.

use std::cmp::{max, min};
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::sync::{Arc, OnceLock};
use std::{ptr, slice};

use crate::grid::GridCell;
use crate::index::Column;
use crate::term::cell::ResetDiscriminant;

/// Un renglón compactado: sabe reconstruir sus casillas sin perder nada.
pub trait ColdRow<T>: std::fmt::Debug + Send + Sync {
    fn unpack(&self) -> Vec<T>;
    fn len(&self) -> usize;
}

/// Renglón compactado y, si alguien lo ha leído, su copia ya reconstruida.
#[derive(Clone, Debug)]
struct Cold<T> {
    packed: Arc<dyn ColdRow<T>>,
    view: OnceLock<Vec<T>>,
}

/// A row in the grid.
#[derive(Clone, Debug)]
pub struct Row<T> {
    /// Casillas; vacío mientras el renglón está compactado.
    inner: Vec<T>,

    /// Maximum number of occupied entries.
    ///
    /// This is the upper bound on the number of elements in the row, which have been modified
    /// since the last reset. All cells after this point are guaranteed to be equal.
    pub(crate) occ: usize,

    /// Aparte y en una caja: un renglón normal solo paga un puntero.
    cold: Option<Box<Cold<T>>>,
}

/// Mismo formato en disco que antes: `{ inner, occ }`.
#[cfg(feature = "serde")]
mod serde_row {
    use super::Row;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    #[serde(rename = "Row")]
    struct Plain<V> {
        inner: V,
        occ: usize,
    }

    impl<T: Serialize> Serialize for Row<T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            Plain { inner: self.cells(), occ: self.occ }.serialize(s)
        }
    }

    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Row<T> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let p = Plain::<Vec<T>>::deserialize(d)?;
            Ok(Row::from_vec(p.inner, p.occ))
        }
    }
}

impl<T> Default for Row<T> {
    fn default() -> Self {
        Row { inner: Vec::new(), occ: 0, cold: None }
    }
}

impl<T> Row<T> {
    #[inline]
    fn cells(&self) -> &Vec<T> {
        match &self.cold {
            None => &self.inner,
            Some(cold) => cold.view.get_or_init(|| cold.packed.unpack()),
        }
    }

    /// Para escribir: el renglón deja de estar compactado.
    #[inline]
    fn cells_mut(&mut self) -> &mut Vec<T> {
        if self.cold.is_some() {
            self.thaw();
        }
        &mut self.inner
    }

    #[inline(never)]
    fn thaw(&mut self) {
        if let Some(cold) = self.cold.take() {
            let Cold { packed, view } = *cold;
            self.inner = view.into_inner().unwrap_or_else(|| packed.unpack());
        }
    }

    /// Compacta el renglón, o suelta la copia que se creó al leerlo.
    pub(crate) fn compact(&mut self)
    where
        T: GridCell,
    {
        match &mut self.cold {
            Some(cold) => cold.view = OnceLock::new(),
            None => {
                if let Some(packed) = T::pack(&self.inner) {
                    self.cold = Some(Box::new(Cold { packed, view: OnceLock::new() }));
                    self.inner = Vec::new();
                }
            },
        }
    }
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.cells() == other.cells()
    }
}

impl<T: Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row { inner, occ: 0, cold: None }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize) {
        if self.len() >= columns {
            return;
        }

        self.cells_mut().resize_with(columns, T::default);
    }

    /// Reduce the number of columns in the row.
    ///
    /// This will return all non-empty cells that were removed.
    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: GridCell,
    {
        if self.len() <= columns {
            return None;
        }

        // Split off cells for a new row.
        let mut new_row = self.cells_mut().split_off(columns);
        let index = new_row.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
        new_row.truncate(index);

        self.occ = min(self.occ, columns);

        if new_row.is_empty() { None } else { Some(new_row) }
    }

    /// Reset all cells in the row to the `template` cell.
    #[inline]
    pub fn reset<D>(&mut self, template: &T)
    where
        T: ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        let len = self.len();
        debug_assert!(len > 0);

        // Mark all cells as dirty if template cell changed.
        if self.cells()[len - 1].discriminant() != template.discriminant() {
            self.occ = len;
        }

        // Reset every dirty cell in the row.
        let occ = self.occ;
        for item in &mut self.cells_mut()[0..occ] {
            item.reset(template);
        }

        self.occ = 0;
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T> {
        Row { inner: vec, occ, cold: None }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.cold {
            None => self.inner.len(),
            Some(cold) => cold.packed.len(),
        }
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.cells().last()
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.occ = self.len();
        self.cells_mut().last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridCell,
    {
        self.occ += vec.len();
        self.cells_mut().append(vec);
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>) {
        self.occ += vec.len();

        let cells = self.cells_mut();
        vec.append(cells);
        *cells = vec;
    }

    /// Check if all cells in the row are empty.
    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridCell,
    {
        self.cells().iter().all(GridCell::is_empty)
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T> {
        self.occ = self.occ.saturating_sub(at);

        let cells = self.cells_mut();
        let mut split = cells.split_off(at);
        std::mem::swap(&mut split, cells);
        split
    }
}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = slice::Iter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> slice::Iter<'a, T> {
        self.cells().iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.occ = self.len();
        self.cells_mut().iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        &self.cells()[index.0]
    }
}

impl<T> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.occ = max(self.occ, *index + 1);
        &mut self.cells_mut()[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.cells()[(index.start.0)..(index.end.0)]
    }
}

impl<T> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.cells_mut()[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.cells()[..(index.end.0)]
    }
}

impl<T> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end);
        &mut self.cells_mut()[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        &self.cells()[(index.start.0)..]
    }
}

impl<T> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.occ = self.len();
        &mut self.cells_mut()[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        &self.cells()[..]
    }
}

impl<T> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.occ = self.len();
        &mut self.cells_mut()[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.cells()[..=(index.end.0)]
    }
}

impl<T> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.occ = max(self.occ, *index.end + 1);
        &mut self.cells_mut()[..=(index.end.0)]
    }
}
