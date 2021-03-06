use std::collections::HashMap;
use std::fmt::{self, Debug};

use base::crypto::{Crypto, Key};
use base::IntoRef;
use error::{Error, Result};
use trans::Eid;
use volume::storage::Storable;
use volume::BLK_SIZE;

/// Mem Storage
#[derive(Clone)]
pub struct MemStorage {
    super_blk_map: HashMap<u64, Vec<u8>>,
    blk_map: HashMap<u64, Vec<u8>>,
    addr_map: HashMap<Eid, Vec<u8>>,
}

impl MemStorage {
    pub fn new() -> Self {
        MemStorage {
            super_blk_map: HashMap::new(),
            blk_map: HashMap::new(),
            addr_map: HashMap::new(),
        }
    }
}

impl Storable for MemStorage {
    #[inline]
    fn exists(&self) -> Result<bool> {
        Ok(false)
    }

    #[inline]
    fn init(&mut self, _crypto: Crypto, _key: Key) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn open(&mut self, _crypto: Crypto, _key: Key) -> Result<()> {
        Ok(())
    }

    fn get_super_block(&mut self, suffix: u64) -> Result<Vec<u8>> {
        self.super_blk_map
            .get(&suffix)
            .map(|b| b.clone())
            .ok_or(Error::NotFound)
    }

    fn put_super_block(&mut self, super_blk: &[u8], suffix: u64) -> Result<()> {
        self.super_blk_map.insert(suffix, super_blk.to_vec());
        Ok(())
    }

    fn get_address(&mut self, id: &Eid) -> Result<Vec<u8>> {
        self.addr_map
            .get(id)
            .map(|addr| addr.clone())
            .ok_or(Error::NotFound)
    }

    fn put_address(&mut self, id: &Eid, addr: &[u8]) -> Result<()> {
        self.addr_map.insert(id.clone(), addr.to_vec());
        Ok(())
    }

    fn del_address(&mut self, id: &Eid) -> Result<()> {
        self.addr_map.remove(id);
        Ok(())
    }

    fn get_blocks(
        &mut self,
        dst: &mut [u8],
        start_idx: u64,
        cnt: usize,
    ) -> Result<()> {
        assert_eq!(dst.len(), BLK_SIZE * cnt);
        let mut read = 0;
        for blk_idx in start_idx..start_idx + cnt as u64 {
            match self.blk_map.get(&blk_idx) {
                Some(blk) => {
                    dst[read..read + BLK_SIZE].copy_from_slice(blk);
                    read += BLK_SIZE;
                }
                None => return Err(Error::NotFound),
            }
        }
        Ok(())
    }

    fn put_blocks(
        &mut self,
        start_idx: u64,
        cnt: usize,
        mut blks: &[u8],
    ) -> Result<()> {
        assert_eq!(blks.len(), BLK_SIZE * cnt);
        for blk_idx in start_idx..start_idx + cnt as u64 {
            self.blk_map.insert(blk_idx, blks[..BLK_SIZE].to_vec());
            blks = &blks[BLK_SIZE..];
        }
        Ok(())
    }

    fn del_blocks(&mut self, start_idx: u64, cnt: usize) -> Result<()> {
        for blk_idx in start_idx..start_idx + cnt as u64 {
            self.blk_map.remove(&blk_idx);
        }
        Ok(())
    }
}

impl Debug for MemStorage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("MemStorage")
            .field("super_blk_map", &self.super_blk_map.len())
            .field("blk_map", &self.blk_map.len())
            .field("addr_map", &self.addr_map.len())
            .finish()
    }
}

impl IntoRef for MemStorage {}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use base::crypto::{Crypto, RandomSeed, RANDOM_SEED_SIZE};
    use base::init_env;
    use base::utils::speed_str;

    #[test]
    fn test_perf() {
        init_env();

        const DATA_LEN: usize = 16 * 1024 * 1024;
        const BLK_CNT: usize = DATA_LEN / BLK_SIZE;
        let mut buf = vec![0u8; DATA_LEN];
        let seed = RandomSeed::from(&[0u8; RANDOM_SEED_SIZE]);
        Crypto::random_buf_deterministic(&mut buf, &seed);

        let mut ms = MemStorage::new();

        // write
        let now = Instant::now();
        ms.put_blocks(0, BLK_CNT, &buf).unwrap();
        let write_time = now.elapsed();

        // read
        let now = Instant::now();
        ms.get_blocks(&mut buf, 0, BLK_CNT).unwrap();
        let read_time = now.elapsed();

        println!(
            "Memory storage (depot) perf: read: {}, write: {}",
            speed_str(&read_time, DATA_LEN),
            speed_str(&write_time, DATA_LEN)
        );
    }
}
