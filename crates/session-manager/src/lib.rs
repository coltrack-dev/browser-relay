use relay_protocol::SessionState;
use serde::Serialize;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("invalid session transition from {0:?}")]
    InvalidTransition(SessionState),
}
#[derive(Debug, Serialize)]
pub struct Session {
    pub state: SessionState,
    pub url: Option<String>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            state: SessionState::Stopped,
            url: None,
        }
    }
}
impl Session {
    pub fn begin_start(&mut self) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Stopped | SessionState::Failed) {
            return Err(SessionError::InvalidTransition(self.state.clone()));
        }
        self.state = SessionState::Starting;
        Ok(())
    }
    pub fn started(&mut self) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Starting) {
            return Err(SessionError::InvalidTransition(self.state.clone()));
        }
        self.state = SessionState::Running;
        Ok(())
    }
    pub fn start_failed(&mut self) {
        self.state = SessionState::Failed;
        self.url = None;
    }
    pub fn begin_stop(&mut self) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Running) {
            return Err(SessionError::InvalidTransition(self.state.clone()));
        }
        self.state = SessionState::Stopping;
        Ok(())
    }
    pub fn stopped(&mut self) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Stopping) {
            return Err(SessionError::InvalidTransition(self.state.clone()));
        }
        self.state = SessionState::Stopped;
        self.url = None;
        Ok(())
    }
    pub fn stop_failed(&mut self) {
        self.state = SessionState::Failed;
    }
    pub fn set_url(&mut self, url: String) -> Result<(), SessionError> {
        if !matches!(self.state, SessionState::Running) {
            return Err(SessionError::InvalidTransition(self.state.clone()));
        }
        self.url = Some(url);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_start_is_restartable() {
        let mut session = Session::default();
        session.begin_start().unwrap();
        session.start_failed();
        session.begin_start().unwrap();
        session.started().unwrap();
        assert!(matches!(session.state, SessionState::Running));
    }
    #[test]
    fn stop_failure_does_not_report_stopped() {
        let mut session = Session::default();
        session.begin_start().unwrap();
        session.started().unwrap();
        session.begin_stop().unwrap();
        session.stop_failed();
        assert!(matches!(session.state, SessionState::Failed));
    }
}
