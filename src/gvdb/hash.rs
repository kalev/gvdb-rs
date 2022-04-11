use crate::gvdb::error::{GvdbError, GvdbResult};
use crate::gvdb::pointer::GvdbPointer;
use crate::gvdb::util::djb_hash;
use safe_transmute::{
    transmute_many_pedantic, transmute_one, transmute_one_pedantic, TriviallyTransmutable,
};
use std::cmp::min;
use std::fmt::{Debug, Formatter};
use std::mem::size_of;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GvdbHashItem {
    hash_value: u32,
    parent: u32,

    key_start: u32,
    key_size: u16,

    typ: u8,
    unused: u8,

    // no endianness here otherwise we would need context
    value: GvdbPointer,
}

unsafe impl TriviallyTransmutable for GvdbHashItem {}

impl GvdbHashItem {
    pub fn hash_value(&self) -> u32 {
        u32::from_le(self.hash_value)
    }

    pub fn parent(&self) -> u32 {
        u32::from_le(self.parent)
    }

    pub fn key_start(&self) -> u32 {
        u32::from_le(self.key_start)
    }

    pub fn key_size(&self) -> u16 {
        u16::from_le(self.key_size)
    }

    pub fn typ(&self) -> char {
        self.typ as char
    }

    pub fn value_ptr(&self) -> &GvdbPointer {
        &self.value
    }
}

#[repr(C)]
#[derive(Copy, Clone, PartialEq)]
pub struct GvdbHashHeader {
    n_bloom_words: u32,
    n_buckets: u32,
}

unsafe impl TriviallyTransmutable for GvdbHashHeader {}

impl GvdbHashHeader {
    pub fn new(n_bloom_words: u32, n_buckets: u32) -> Self {
        Self {
            n_bloom_words,
            n_buckets,
        }
    }

    pub fn n_bloom_words(&self) -> u32 {
        u32::from_le(self.n_bloom_words) & (1 << 27) - 1
    }

    pub fn n_buckets(&self) -> u32 {
        u32::from_le(self.n_buckets)
    }
}

impl Debug for GvdbHashHeader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GvdbHashHeader {{ n_bloom_words: {}, n_buckets: {} }}",
            self.n_bloom_words(),
            self.n_buckets()
        )
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct GvdbHashTable<'a> {
    data: &'a [u8],
    table_ptr: GvdbPointer,
    header: GvdbHashHeader,
}

impl<'a> GvdbHashTable<'a> {
    pub fn for_bytes(data: &'a [u8], table_ptr: GvdbPointer) -> GvdbResult<Self> {
        let header = Self::hash_header(data, &table_ptr)?;

        let this = Self {
            data,
            table_ptr,
            header,
        };

        let table_data = this.deref_pointer(&this.table_ptr, 4)?;

        if this.bloom_words_offset() > table_data.len()
            || this.hash_buckets_offset() > table_data.len()
            || this.hash_items_offset() > table_data.len()
        {
            Err(GvdbError::DataOffset)
        } else if (table_data.len() - this.hash_items_offset()) % size_of::<GvdbHashItem>() != 0 {
            // Wrong data length
            Err(GvdbError::DataError(format!(
                "Remaining size invalid: Expected a multiple of {}, got {}",
                size_of::<GvdbHashItem>(),
                table_data.len()
            )))
        } else {
            Ok(this)
        }
    }

    pub fn hash_header(data: &'a [u8], pointer: &GvdbPointer) -> GvdbResult<GvdbHashHeader> {
        let start = pointer.start() as usize;
        let bytes: &[u8] = data
            .get(start..start + size_of::<GvdbHashHeader>())
            .ok_or(GvdbError::DataOffset)?;

        Ok(transmute_one(bytes)?)
    }

    /// gvdb_table_dereference
    fn deref_pointer(&self, pointer: &GvdbPointer, alignment: u32) -> GvdbResult<&[u8]> {
        let start: usize = pointer.start() as usize;
        let end: usize = pointer.end() as usize;
        let alignment: usize = alignment.try_into()?;

        if start > end {
            Err(GvdbError::DataOffset)
        } else if start & (alignment - 1) != 0 {
            Err(GvdbError::DataAlignment)
        } else {
            self.data.get(start..end).ok_or(GvdbError::DataOffset)
        }
    }

    fn get_u32(&self, offset: usize) -> Option<u32> {
        let bytes = self.data.get(offset..offset + size_of::<u32>())?;
        Some(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn data_offset(&self) -> usize {
        self.table_ptr.start() as usize
    }

    fn bloom_words_offset(&self) -> usize {
        self.data_offset() + size_of::<GvdbHashHeader>()
    }

    fn bloom_words_end(&self) -> usize {
        self.bloom_words_offset() + self.header.n_bloom_words() as usize * size_of::<u32>()
    }

    pub fn bloom_words(&self) -> Option<&[u32]> {
        let data_u8 = self
            .data
            .get(self.bloom_words_offset()..self.bloom_words_end());
        if let Some(data_u8) = data_u8 {
            transmute_many_pedantic(data_u8).ok()
        } else {
            None
        }
    }

    fn get_bloom_word(&self, index: usize) -> Option<u32> {
        let start = self.bloom_words_offset() + index * size_of::<u32>();
        self.get_u32(start)
    }

    // TODO: Calculate proper bloom shift
    fn bloom_shift(&self) -> usize {
        0
    }

    /// gvdb_table_bloom_filter
    pub fn bloom_filter(&self, hash_value: u32) -> bool {
        if self.header.n_bloom_words() == 0 {
            return true;
        }

        let word = (hash_value / 32) % self.header.n_bloom_words();
        let mut mask = 1 << (hash_value & 31);
        mask |= 1 << ((hash_value >> self.bloom_shift()) & 31);

        // We know this index is < n_bloom_words
        let bloom_word = self.get_bloom_word(word as usize).unwrap();
        bloom_word & mask == mask
    }

    fn hash_buckets_offset(&self) -> usize {
        self.bloom_words_end()
    }

    fn hash_buckets_end(&self) -> usize {
        self.hash_buckets_offset() + self.header.n_buckets as usize * size_of::<u32>()
    }

    fn get_hash(&self, index: usize) -> Option<u32> {
        let start = self.hash_buckets_offset() + index * size_of::<u32>();
        self.get_u32(start)
    }

    fn hash_items_offset(&self) -> usize {
        self.hash_buckets_end()
    }

    fn n_hash_items(&self) -> usize {
        let len = self.table_ptr.end() as usize - self.hash_items_offset();
        len / size_of::<GvdbHashItem>()
    }

    fn hash_items_end(&self) -> usize {
        self.data.len()
    }

    /// gvdb_table_item_get_key
    pub fn get_key(&self, item: &GvdbHashItem) -> GvdbResult<String> {
        let start = item.key_start() as usize;
        let size = item.key_size() as usize;
        let end = start + size;

        let data = self.data.get(start..end).ok_or(GvdbError::DataOffset)?;
        Ok(String::from_utf8(data.to_vec())?)
    }

    /// Get the hash item at hash item index
    fn get_hash_item(&self, index: usize) -> GvdbResult<GvdbHashItem> {
        let size = size_of::<GvdbHashItem>();
        let start = self.hash_items_offset() + size * index;
        let end = start + size;

        let data = self.data.get(start..end).ok_or(GvdbError::DataOffset)?;
        Ok(transmute_one_pedantic(data)?)
    }

    /// Gets a list of keys
    pub fn get_names(&self) -> GvdbResult<Vec<String>> {
        let count = self.n_hash_items();
        let mut names = vec![None; count];

        let mut inserted = 0;
        while inserted < count {
            let last_inserted = inserted;
            for index in 0..count as usize {
                let item = self.get_hash_item(index)?;
                let parent: usize = item.parent().try_into()?;
                if parent == 0xffffffff {
                    // root item
                    let name = self.get_key(&item)?;
                    let _ = std::mem::replace(&mut names[index], Some(name));
                    inserted += 1;
                } else if parent < count && names[parent].is_some() {
                    // We already came across this item
                    let name = self.get_key(&item)?;
                    let parent_name = names.get(parent).unwrap().as_ref().unwrap();
                    let full_name = name + parent_name;
                    let _ = std::mem::replace(&mut names[index], Some(full_name));
                    inserted += 1;
                } else if parent > count {
                    return Err(GvdbError::DataError(format!(
                        "Parent with invalid offset encountered: {}",
                        parent
                    )));
                }
            }

            if last_inserted == inserted {
                // No insertion took place this round, there must be a parent loop
                // We fail instead of infinitely looping
                return Err(GvdbError::InvalidData);
            }
        }

        let names = names.into_iter().map(|s| s.unwrap()).collect();
        Ok(names)
    }

    fn check_name(&self, item: &GvdbHashItem, key: &str) -> bool {
        let this_key = match self.get_key(item) {
            Ok(this_key) => this_key,
            Err(_) => return false,
        };

        if key != this_key {
            return false;
        }

        let parent = item.parent();
        if key.len() == this_key.len() && parent == 0xffffffff {
            return true;
        }

        if parent < self.n_hash_items() as u32 && key.len() > 0 {
            let parent_item = match self.get_hash_item(parent as usize) {
                Ok(p) => p,
                Err(_) => return false,
            };

            return self.check_name(&parent_item, key);
        }

        false
    }

    fn table_lookup(&self, key: &str, typ: char) -> Option<GvdbHashItem> {
        if self.header.n_buckets == 0 || self.n_hash_items() == 0 {
            return None;
        }

        let hash_value = djb_hash(key);
        if !self.bloom_filter(hash_value) {
            return None;
        }

        let bucket = hash_value % self.header.n_buckets;
        let mut itemno = self.get_hash(bucket as usize)? as usize;

        let lastno = if bucket == self.header.n_buckets - 1 {
            self.n_hash_items() as usize
        } else {
            min(
                self.get_hash(bucket as usize + 1)?,
                self.n_hash_items() as u32,
            ) as usize
        };

        while itemno < lastno {
            let item = self.get_hash_item(itemno).ok()?;
            if hash_value == item.hash_value() {
                if self.check_name(&item, key) {
                    if item.typ() == typ {
                        return Some(item);
                    }
                }
            }

            itemno += 1;
        }

        None
    }

    pub fn value_from_item(&self, item: &GvdbHashItem) -> Option<glib::Variant> {
        let data: &[u8] = self.deref_pointer(&item.value_ptr(), 8).ok()?;
        Some(glib::Variant::from_data_with_type(
            data,
            glib::VariantTy::VARIANT,
        ))
    }

    pub fn get_value(&self, key: &str) -> Option<glib::Variant> {
        let item = self.table_lookup(key, 'v')?;
        self.value_from_item(&item)
    }

    /*
        self.bloom_words_offset = pointer.start() as usize + size_of::<GvdbHashHeader>();
        self.n_bloom_words = header.n_bloom_words();
        size -= self.n_bloom_words as usize * size_of::<u32>();

        self.hash_buckets_offset =
        self.bloom_words_offset + self.n_bloom_words as usize * size_of::<u32>();
        self.n_buckets = header.n_buckets();
        size -= self.n_buckets as usize * size_of::<u32>();

        self.hash_items_offset =
        self.hash_buckets_offset + self.n_buckets as usize * size_of::<u32>();
        self.n_hash_items = (size / size_of::<GvdbHashItem>()) as u32;
        if size % size_of::<GvdbHashItem>() != 0 {
        return Err(GvdbError::DataError(format!(
        "Remaining size invalid: Expected a multiple of {}, got {}",
        size_of::<GvdbHashItem>(),
        size
        )));
        }
    */
}
