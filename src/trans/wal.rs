use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{self, Debug};
use std::hash::{Hash, Hasher};

use super::trans::Action;
use super::{Eid, Id, Txid};
use base::crypto::{HashKey, HASHKEY_SIZE};
use error::{Error, Result};
use volume::{
    AllocatorRef, Arm, ArmAccess, Armor, Seq, VolumeArmor, VolumeRef,
};

/// Wal entry entity type
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum EntityType {
    Cow,
    Direct,
}

/// Wal entry
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    id: Eid,
    action: Action,
    ent_type: EntityType,
    arm: Arm,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Entry) -> bool {
        self.id == other.id
    }
}

impl Eq for Entry {}

/// Write Ahead Log (WAL)
#[derive(Debug, Clone, Default, Eq, Deserialize, Serialize)]
pub struct Wal {
    id: Eid,
    seq: u64,
    arm: Arm,
    txid: Txid,
    entries: HashMap<Eid, Entry>,
}

impl Wal {
    // hash key for wal id derivation
    const ID_HASH_KEY: [u8; HASHKEY_SIZE] = [42u8; HASHKEY_SIZE];

    pub fn new(txid: Txid) -> Self {
        Wal {
            id: Self::derive_id(txid),
            seq: 0,
            arm: Arm::default(),
            txid,
            entries: HashMap::new(),
        }
    }

    // derive wal id from txid
    fn derive_id(txid: Txid) -> Eid {
        let mut hash_key = HashKey::new_empty();
        hash_key.copy(&Self::ID_HASH_KEY[..]);
        txid.derive_id(&hash_key)
    }

    #[inline]
    pub fn add_entry(
        &mut self,
        id: &Eid,
        action: Action,
        ent_type: EntityType,
        arm: Arm,
    ) {
        self.entries.insert(
            id.clone(),
            Entry {
                id: id.clone(),
                action,
                ent_type,
                arm,
            },
        );
    }

    #[inline]
    pub fn remove_entry(&mut self, id: &Eid) {
        self.entries.remove(id);
    }

    // recylce a wal
    fn recyle(&self, wal_armor: &VolumeArmor<Self>) -> Result<()> {
        debug!("recycle tx#{}", self.txid);
        for ent in self.entries.values() {
            match ent.action {
                Action::New | Action::Update => {} // do nothing
                Action::Delete => {
                    wal_armor.remove_all_arms(&ent.id)?;
                }
            }
        }
        Ok(())
    }

    // clean each aborted entry in wal
    pub fn clean_aborted(&self, vol: &VolumeRef) -> Result<()> {
        for ent in self.entries.values() {
            match ent.action {
                Action::New => match ent.ent_type {
                    EntityType::Cow => Arm::remove_all(&ent.id, vol)?,
                    EntityType::Direct => {
                        let mut vol = vol.write().unwrap();
                        vol.del(&ent.id)?;
                    }
                },
                Action::Update => match ent.ent_type {
                    EntityType::Cow => ent.arm.remove_arm(&ent.id, vol)?,
                    EntityType::Direct => {
                        let mut vol = vol.write().unwrap();
                        vol.del(&ent.id)?;
                    }
                },
                Action::Delete => {} // do nothing
            }
        }
        Ok(())
    }
}

impl Hash for Wal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.txid.val().hash(state);
    }
}

impl PartialEq for Wal {
    fn eq(&self, other: &Wal) -> bool {
        self.txid == other.txid
    }
}

impl Id for Wal {
    #[inline]
    fn id(&self) -> &Eid {
        &self.id
    }

    #[inline]
    fn id_mut(&mut self) -> &mut Eid {
        &mut self.id
    }
}

impl Seq for Wal {
    #[inline]
    fn seq(&self) -> u64 {
        self.seq
    }

    #[inline]
    fn inc_seq(&mut self) {
        self.seq += 1
    }
}

impl<'de> ArmAccess<'de> for Wal {
    #[inline]
    fn arm(&self) -> Arm {
        self.arm
    }

    #[inline]
    fn arm_mut(&mut self) -> &mut Arm {
        &mut self.arm
    }
}

/// Wal queue
///
/// The whole wal queue should be able to fit into one block, so
/// the persisted size should less than one block size.
#[derive(Default, Clone, Deserialize, Serialize)]
struct WalQueue {
    id: Eid,
    seq: u64,
    arm: Arm,

    // txid and block watermark
    txid_wmark: u64,
    blk_wmark: u64,

    // completed tx queue
    done: VecDeque<Txid>,

    // in-progress tx id list
    doing: HashSet<Txid>,

    #[serde(skip_serializing, skip_deserializing, default)]
    aborting: HashMap<Txid, Wal>,

    #[serde(skip_serializing, skip_deserializing, default)]
    wal_armor: VolumeArmor<Wal>,
}

impl WalQueue {
    const COMMITTED_QUEUE_SIZE: usize = 2;

    pub fn new(id: &Eid, vol: &VolumeRef) -> Self {
        WalQueue {
            id: id.clone(),
            seq: 0,
            arm: Arm::default(),
            txid_wmark: 0,
            blk_wmark: 0,
            done: VecDeque::new(),
            doing: HashSet::new(),
            aborting: HashMap::new(),
            wal_armor: VolumeArmor::new(vol),
        }
    }

    #[inline]
    fn watermarks(&self) -> (u64, u64) {
        (self.txid_wmark, self.blk_wmark)
    }

    #[inline]
    fn set_watermarks(&mut self, txid_wmark: u64, blk_wmark: u64) {
        self.txid_wmark = txid_wmark;
        self.blk_wmark = blk_wmark;
    }

    #[inline]
    fn has_doing(&self) -> bool {
        !self.doing.is_empty()
    }

    #[inline]
    fn has_abort(&self) -> bool {
        !self.aborting.is_empty()
    }

    #[inline]
    fn open(&mut self, vol: &VolumeRef) {
        self.wal_armor = VolumeArmor::new(vol);
    }

    #[inline]
    fn begin_trans(&mut self, txid: Txid) {
        assert!(!self.doing.contains(&txid));
        self.doing.insert(txid);
    }

    fn commit_trans(&mut self, wal: Wal) -> Result<()> {
        // recycle the retired trans
        while self.done.len() >= Self::COMMITTED_QUEUE_SIZE {
            {
                // get retiree from end of queue
                let retiree_txid = self.done.front().unwrap();
                let retiree_id = Wal::derive_id(*retiree_txid);

                // load the retired wal
                match self.wal_armor.load_item(&retiree_id) {
                    Ok(retiree) => {
                        // recycle and remove the wal
                        retiree.recyle(&self.wal_armor)?;
                        self.wal_armor.remove_all_arms(&retiree_id)?;
                    }
                    Err(ref err) if *err == Error::NotFound => {
                        // wal is already recycled and removed, do nothing
                        // here but skip it
                    }
                    Err(err) => return Err(err),
                }
            }

            self.done.pop_front();
        }

        // remove txid from doing list and enqueue it
        self.doing.remove(&wal.txid);
        self.done.push_back(wal.txid);

        Ok(())
    }

    #[inline]
    fn begin_abort(&mut self, wal: &Wal) {
        self.aborting.insert(wal.txid, wal.clone());
    }

    #[inline]
    fn end_abort(&mut self, txid: Txid) {
        self.aborting.remove(&txid);
        self.doing.remove(&txid);
    }

    // hot redo failed abort
    fn hot_redo_abort(&mut self, vol: &VolumeRef) -> Result<()> {
        let mut completed = Vec::new();

        for wal in self.aborting.values() {
            debug!("hot redo abort tx#{}", wal.txid);
            wal.clean_aborted(vol)?;
            completed.push(wal.txid);
        }

        // remove all txs which are completed to retry abort
        for txid in completed.iter() {
            self.end_abort(*txid);
        }

        Ok(())
    }

    // cold redo failed abort
    fn cold_redo_abort(&mut self, vol: &VolumeRef) -> Result<()> {
        let mut completed = Vec::new();

        for txid in &self.doing {
            debug!("cold redo abort tx#{}", txid);
            let wal_id = Wal::derive_id(*txid);
            if let Ok(wal) = self.wal_armor.load_item(&wal_id) {
                wal.clean_aborted(vol)?;
            }
            completed.push(*txid);
        }

        // remove all txs which are succeed to retry abort
        for txid in completed.iter() {
            self.end_abort(*txid);
        }

        Ok(())
    }
}

impl Debug for WalQueue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("WalQueue")
            .field("id", &self.id)
            .field("seq", &self.seq)
            .field("arm", &self.arm)
            .field("done", &self.done)
            .field("doing", &self.doing)
            .field("aborting", &self.aborting)
            .finish()
    }
}

impl Id for WalQueue {
    #[inline]
    fn id(&self) -> &Eid {
        &self.id
    }

    #[inline]
    fn id_mut(&mut self) -> &mut Eid {
        &mut self.id
    }
}

impl Seq for WalQueue {
    #[inline]
    fn seq(&self) -> u64 {
        self.seq
    }

    #[inline]
    fn inc_seq(&mut self) {
        self.seq += 1
    }
}

impl<'de> ArmAccess<'de> for WalQueue {
    #[inline]
    fn arm(&self) -> Arm {
        self.arm
    }

    #[inline]
    fn arm_mut(&mut self) -> &mut Arm {
        &mut self.arm
    }
}

/// WalQueue Manager
#[derive(Default)]
pub struct WalQueueMgr {
    // txid watermark
    txid_wmark: Txid,

    // wal queue and wal queue armor
    walq: WalQueue,
    walq_backup: Option<WalQueue>,
    walq_armor: VolumeArmor<WalQueue>,

    // block allocator
    allocator: AllocatorRef,
}

impl WalQueueMgr {
    pub fn new(walq_id: &Eid, vol: &VolumeRef) -> Self {
        let allocator = {
            let vol = vol.read().unwrap();
            vol.allocator()
        };
        WalQueueMgr {
            txid_wmark: Txid::from(0),
            walq: WalQueue::new(walq_id, vol),
            walq_backup: None,
            walq_armor: VolumeArmor::new(vol),
            allocator,
        }
    }

    pub fn open(&mut self, walq_id: &Eid, vol: &VolumeRef) -> Result<()> {
        // load wal queue
        self.walq = self.walq_armor.load_item(walq_id)?;
        self.walq.open(vol);

        // restore water marks
        let (txid_wmark, blk_wmark) = self.walq.watermarks();
        self.txid_wmark = Txid::from(txid_wmark);
        {
            let mut allocator = self.allocator.write().unwrap();
            allocator.set_block_wmark(blk_wmark);
        }

        // now redo abort tx if any
        if self.walq.has_doing() {
            self.backup_walq();
            self.walq.cold_redo_abort(vol).or_else(|err| {
                // if failed, restore the walq backup
                self.restore_walq();
                Err(err)
            })?;
            self.save_walq()?;
            debug!("cold abort completed");
        }

        debug!(
            "WalQueue opened, seq: {}, watermarks: txid: {}, block: {}",
            self.walq.seq(),
            txid_wmark,
            blk_wmark
        );

        Ok(())
    }

    #[inline]
    pub fn next_txid(&mut self) -> Txid {
        self.txid_wmark.next()
    }

    #[inline]
    fn backup_walq(&mut self) {
        self.walq_backup = Some(self.walq.clone());
    }

    #[inline]
    fn restore_walq(&mut self) {
        self.walq = self.walq_backup.take().unwrap();
    }

    fn save_walq(&mut self) -> Result<()> {
        // reserve one block beforehand for the wal queue, because wal queue
        // itself will consume a block
        let blk_wmark = {
            let mut allocator = self.allocator.write().unwrap();
            allocator.reserve(1)
        };

        // save watermarks to wal queue and save it,
        self.walq.set_watermarks(self.txid_wmark.val(), blk_wmark);
        self.walq_armor.save_item(&mut self.walq).or_else(|err| {
            // if save failed, restore the walq backup
            self.restore_walq();
            Err(err)
        })?;

        // make sure the block watermark is correct
        {
            let allocator = self.allocator.read().unwrap();
            assert_eq!(allocator.block_wmark(), blk_wmark);
        }

        Ok(())
    }

    pub fn begin_trans(&mut self, txid: Txid) -> Result<()> {
        self.backup_walq();
        self.walq.begin_trans(txid);
        self.save_walq()
    }

    pub fn commit_trans(&mut self, wal: Wal) -> Result<()> {
        self.backup_walq();
        self.walq.commit_trans(wal).or_else(|err| {
            // if commit failed, restore the walq backup
            self.restore_walq();
            Err(err)
        })?;
        self.save_walq()
    }

    #[inline]
    pub fn begin_abort(&mut self, wal: &Wal) {
        self.walq.begin_abort(wal)
    }

    pub fn end_abort(&mut self, txid: Txid) -> Result<()> {
        self.backup_walq();
        self.walq.end_abort(txid);
        self.save_walq()
    }

    pub fn hot_redo_abort(&mut self, vol: &VolumeRef) -> Result<()> {
        if !self.walq.has_abort() {
            return Ok(());
        }

        self.backup_walq();
        self.walq.hot_redo_abort(vol).or_else(|err| {
            // if failed, restore the walq backup
            self.restore_walq();
            Err(err)
        })?;
        self.save_walq().and_then(|_| {
            debug!("hot abort completed");
            Ok(())
        })
    }
}

impl Debug for WalQueueMgr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("WalQueueMgr")
            .field("txid_wmark", &self.txid_wmark)
            .field("walq", &self.walq)
            .finish()
    }
}
