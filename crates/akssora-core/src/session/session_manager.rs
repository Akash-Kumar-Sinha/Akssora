use std::collections::HashMap;

use crate::{
    error::Result,
    session::{Session, SessionId},
};

pub struct SessionManager {
    pub sessions: HashMap<SessionId, Session>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    pub async fn create_session(&mut self) -> Result<SessionId> {
        let session = Session::new().await?;
        let session_id = session.session_id;

        self.sessions.insert(session_id, session);

        Ok(session_id)
    }

    pub fn session_ids(&self) -> impl Iterator<Item = &SessionId> {
        self.sessions.keys()
    }

    pub fn get_session(&self, session_id: &SessionId) -> Option<&Session> {
        self.sessions.get(session_id)
    }

    pub fn get_session_mut(&mut self, session_id: &SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(session_id)
    }

    pub async fn end_session(&mut self, session_id: &SessionId) -> Result<()> {
        let session = self
            .sessions
            .remove(session_id)
            .ok_or_else(|| crate::error::AkssoraCoreError::SessionNotFound(*session_id))?;

        session.end().await?;

        Ok(())
    }

    pub fn contains(&self, session_id: &SessionId) -> bool {
        self.sessions.contains_key(session_id)
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
