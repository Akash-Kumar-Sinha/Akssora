
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SnapshotVisibility {
    #[default]
    Private,
    OrgShared,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: Uuid,
    pub owner_account_id: Uuid,
    pub source_sandbox_id: Uuid,
    pub name: String,
    pub description: String,
    pub visibility: SnapshotVisibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub shared_with: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareGrant {
    pub snapshot_id: Uuid,
    pub granted_account_id: Uuid,
    pub granted_by: Uuid,
    pub granted_at: DateTime<Utc>,
}

pub struct SnapshotSharingStore {
    snapshots: dashmap::DashMap<Uuid, SnapshotMeta>,
    grants: dashmap::DashMap<(Uuid, Uuid), ShareGrant>,
    shared_with_account: dashmap::DashMap<Uuid, Vec<Uuid>>,
}

impl SnapshotSharingStore {
    pub fn new() -> Self {
        Self {
            snapshots: dashmap::DashMap::new(),
            grants: dashmap::DashMap::new(),
            shared_with_account: dashmap::DashMap::new(),
        }
    }

    pub fn register(
        &self,
        id: Uuid,
        owner_account_id: Uuid,
        source_sandbox_id: Uuid,
        name: &str,
        description: &str,
    ) -> SnapshotMeta {
        let meta = SnapshotMeta {
            id,
            owner_account_id,
            source_sandbox_id,
            name: name.to_string(),
            description: description.to_string(),
            visibility: SnapshotVisibility::Private,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            shared_with: Vec::new(),
        };

        self.snapshots.insert(id, meta.clone());
        meta
    }

    pub fn get(&self, snapshot_id: &Uuid) -> Option<SnapshotMeta> {
        self.snapshots.get(snapshot_id).map(|m| m.value().clone())
    }

    pub fn share_with_org(
        &self,
        snapshot_id: &Uuid,
        owner_account_id: &Uuid,
    ) -> Result<SnapshotMeta, ShareError> {
        let mut meta = self
            .snapshots
            .get_mut(snapshot_id)
            .ok_or(ShareError::SnapshotNotFound)?;

        if meta.owner_account_id != *owner_account_id {
            return Err(ShareError::NotOwner);
        }

        meta.visibility = SnapshotVisibility::OrgShared;
        meta.updated_at = Utc::now();

        tracing::info!(
            snapshot_id = %snapshot_id,
            owner = %owner_account_id,
            "snapshot shared with org"
        );

        Ok(meta.clone())
    }

    pub fn share_with_account(
        &self,
        snapshot_id: &Uuid,
        owner_account_id: &Uuid,
        granted_account_id: &Uuid,
    ) -> Result<ShareGrant, ShareError> {
        {
            let meta = self
                .snapshots
                .get(snapshot_id)
                .ok_or(ShareError::SnapshotNotFound)?;

            if meta.owner_account_id != *owner_account_id {
                return Err(ShareError::NotOwner);
            }
        }

        if owner_account_id == granted_account_id {
            return Err(ShareError::CannotShareWithSelf);
        }

        let grant = ShareGrant {
            snapshot_id: *snapshot_id,
            granted_account_id: *granted_account_id,
            granted_by: *owner_account_id,
            granted_at: Utc::now(),
        };

        self.grants
            .insert((*snapshot_id, *granted_account_id), grant.clone());

        if let Some(mut meta) = self.snapshots.get_mut(snapshot_id)
            && !meta.shared_with.contains(granted_account_id) {
                meta.shared_with.push(*granted_account_id);
                meta.updated_at = Utc::now();
            }

        self.shared_with_account
            .entry(*granted_account_id)
            .or_default()
            .push(*snapshot_id);

        tracing::info!(
            snapshot_id = %snapshot_id,
            owner = %owner_account_id,
            granted_to = %granted_account_id,
            "snapshot shared with specific account"
        );

        Ok(grant)
    }

    pub fn can_access(&self, snapshot_id: &Uuid, account_id: &Uuid) -> bool {
        let meta = match self.snapshots.get(snapshot_id) {
            Some(m) => m.value().clone(),
            None => return false,
        };

        if meta.owner_account_id == *account_id {
            return true;
        }

        if meta.visibility == SnapshotVisibility::OrgShared
            && meta.owner_account_id == *account_id
        {
            return true;
        }

        self.grants.contains_key(&(*snapshot_id, *account_id))
    }

    pub fn list_visible(&self, account_id: &Uuid) -> Vec<SnapshotMeta> {
        self.snapshots
            .iter()
            .filter(|entry| {
                let meta = entry.value();
                if meta.owner_account_id == *account_id {
                    return true;
                }
                if meta.visibility == SnapshotVisibility::OrgShared
                    && meta.owner_account_id == *account_id
                {
                    return true;
                }
                meta.shared_with.contains(account_id)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn list_shared_with(&self, account_id: &Uuid) -> Vec<SnapshotMeta> {
        self.snapshots
            .iter()
            .filter(|entry| {
                let meta = entry.value();
                meta.shared_with.contains(account_id)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    pub fn remove(&self, snapshot_id: &Uuid) -> Option<SnapshotMeta> {
        let meta = self.snapshots.remove(snapshot_id).map(|(_, m)| m)?;

        for account_id in &meta.shared_with {
            self.grants.remove(&(*snapshot_id, *account_id));
            if let Some(mut list) = self.shared_with_account.get_mut(account_id) {
                list.retain(|id| id != snapshot_id);
            }
        }

        Some(meta)
    }

    pub fn count(&self) -> usize {
        self.snapshots.len()
    }
}

impl Default for SnapshotSharingStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("snapshot not found")]
    SnapshotNotFound,

    #[error("only the owner can share a snapshot")]
    NotOwner,

    #[error("cannot share a snapshot with yourself")]
    CannotShareWithSelf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_default_private() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let meta = store.register(Uuid::new_v4(), owner, Uuid::new_v4(), "test", "desc");
        assert_eq!(meta.visibility, SnapshotVisibility::Private);
    }

    #[test]
    fn share_with_org() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");

        let meta = store.share_with_org(&snapshot_id, &owner).unwrap();
        assert_eq!(meta.visibility, SnapshotVisibility::OrgShared);
    }

    #[test]
    fn non_owner_cannot_share() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");

        let result = store.share_with_org(&snapshot_id, &Uuid::new_v4());
        assert!(result.is_err());
    }

    #[test]
    fn share_with_specific_account() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let friend = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");

        let grant = store.share_with_account(&snapshot_id, &owner, &friend).unwrap();
        assert_eq!(grant.granted_account_id, friend);
        assert!(store.can_access(&snapshot_id, &friend));
    }

    #[test]
    fn cannot_share_with_self() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");

        let result = store.share_with_account(&snapshot_id, &owner, &owner);
        assert!(result.is_err());
    }

    #[test]
    fn can_access_checks() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");

        assert!(store.can_access(&snapshot_id, &owner));
        assert!(!store.can_access(&snapshot_id, &stranger));
    }

    #[test]
    fn list_visible_includes_own() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        store.register(Uuid::new_v4(), owner, Uuid::new_v4(), "s1", "");
        store.register(Uuid::new_v4(), owner, Uuid::new_v4(), "s2", "");

        let visible = store.list_visible(&owner);
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn remove_cleans_up() {
        let store = SnapshotSharingStore::new();
        let owner = Uuid::new_v4();
        let friend = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        store.register(snapshot_id, owner, Uuid::new_v4(), "test", "desc");
        store.share_with_account(&snapshot_id, &owner, &friend).unwrap();

        assert!(store.remove(&snapshot_id).is_some());
        assert!(!store.can_access(&snapshot_id, &friend));
    }
}
