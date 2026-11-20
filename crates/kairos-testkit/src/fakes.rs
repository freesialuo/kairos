use kairos_core::{
    CoreError, DbError, NetError,
    ports::{Alarm, Clock, Commit, Entropy, Http, OutRequest, OutResponse},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::{pending, poll_fn},
    rc::Rc,
    task::Poll,
};

#[derive(Default)]
struct FaultState {
    error: Option<String>,
    panic: Option<String>,
    kill: Option<String>,
    halt: bool,
    commit_error: bool,
    yield_commit: bool,
}

/// Faults persist until replaced. A simulated kill affects only the next I/O
/// boundary, so a subsequent invocation can observe the durable trace.
#[derive(Default, Clone)]
pub struct Faults(Rc<RefCell<FaultState>>);
impl Faults {
    pub fn fail_sql_matching(&self, s: &str) {
        self.0.borrow_mut().error = Some(s.into());
    }
    pub fn panic_sql_matching(&self, s: &str) {
        self.0.borrow_mut().panic = Some(s.into());
    }
    pub fn kill_after_sql_matching(&self, s: &str) {
        self.0.borrow_mut().kill = Some(s.into());
    }
    pub fn fail_next_commit(&self) {
        self.0.borrow_mut().commit_error = true;
    }
    pub fn yield_next_commit(&self) {
        self.0.borrow_mut().yield_commit = true;
    }
    pub(crate) fn before_sql(&self, sql: &str) -> Result<(), DbError> {
        let state = self.0.borrow();
        assert!(
            !state.panic.as_ref().is_some_and(|s| sql.contains(s)),
            "injected SQL panic"
        );
        if state.error.as_ref().is_some_and(|s| sql.contains(s)) {
            return Err(DbError("injected SQL error".into()));
        }
        Ok(())
    }
    pub(crate) fn after_sql(&self, sql: &str) {
        let mut state = self.0.borrow_mut();
        if state.kill.as_ref().is_some_and(|s| sql.contains(s)) {
            state.halt = true;
            state.kill = None;
        }
    }
    async fn boundary(&self) {
        let halt = std::mem::take(&mut self.0.borrow_mut().halt);
        if halt {
            pending::<()>().await
        }
    }
}

pub struct FakeClock(pub Cell<i64>);
impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.get()
    }
}

/// Test-only deterministic byte stream. State is retained between calls.
pub struct SeededEntropy(pub Cell<u64>);
impl Entropy for SeededEntropy {
    fn fill(&self, buf: &mut [u8]) {
        let mut state = self.0.get();
        for byte in buf {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        self.0.set(state);
    }
}

#[derive(Default)]
pub struct FakeAlarm {
    pub(crate) at: Cell<Option<i64>>,
    pub(crate) calls: Cell<usize>,
    fail_set: Cell<bool>,
}
impl FakeAlarm {
    pub fn fail_next_set(&self) {
        self.fail_set.set(true);
    }
}
impl Alarm for FakeAlarm {
    async fn get(&self) -> Result<Option<i64>, CoreError> {
        Ok(self.at.get())
    }
    async fn set(&self, at_ms: i64) -> Result<(), CoreError> {
        if self.fail_set.replace(false) {
            return Err(DbError("injected set alarm failure".into()).into());
        }
        self.at.set(Some(at_ms));
        self.calls.set(self.calls.get() + 1);
        Ok(())
    }
}
pub struct FakeCommit(pub(crate) Faults);
impl Commit for FakeCommit {
    async fn commit_point(&self) -> Result<(), CoreError> {
        self.0.boundary().await;
        let Faults(state) = &self.0;
        let mut should_yield = std::mem::take(&mut state.borrow_mut().yield_commit);
        poll_fn(|cx| {
            if std::mem::take(&mut should_yield) {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        if std::mem::take(&mut state.borrow_mut().commit_error) {
            Err(DbError("injected commit failure".into()).into())
        } else {
            Ok(())
        }
    }
}

type Reply = (String, Result<OutResponse, NetError>);
#[derive(Default)]
pub struct FakeHttp {
    replies: RefCell<VecDeque<Reply>>,
    sent: RefCell<Vec<OutRequest>>,
    pub(crate) faults: Faults,
}
impl FakeHttp {
    pub(crate) fn new(faults: Faults) -> Self {
        Self {
            faults,
            ..Self::default()
        }
    }
    pub fn respond(&self, prefix: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        self.replies.borrow_mut().push_back((
            prefix.into(),
            Ok(OutResponse {
                status,
                headers: headers.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
                body: body.to_vec(),
            }),
        ));
    }
    pub fn fail(&self, prefix: &str, error: NetError) {
        self.replies
            .borrow_mut()
            .push_back((prefix.into(), Err(error)));
    }
    pub fn requests(&self) -> Vec<OutRequest> {
        self.sent.borrow().clone()
    }
}
impl Http for FakeHttp {
    async fn send(&self, request: OutRequest) -> Result<OutResponse, NetError> {
        self.faults.boundary().await;
        let cap = request.max_body_bytes;
        let mut replies = self.replies.borrow_mut();
        let index = replies
            .iter()
            .position(|(prefix, _)| request.url.starts_with(prefix));
        self.sent.borrow_mut().push(request);
        let (_, reply) = index
            .and_then(|i| replies.remove(i))
            .ok_or_else(|| NetError::Network("no scripted HTTP response".into()))?;
        let response = reply?;
        if response.body.len() > cap {
            return Err(NetError::TooLarge);
        }
        Ok(response)
    }
}
