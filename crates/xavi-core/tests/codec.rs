use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use xavi_core::codec::{Codec, CodecState, Engine, Receive, support};
use xavi_core::stream::Limits;
use xavi_core::{Error, ErrorKind, Result};

#[derive(Default)]
struct Control {
    opens: usize,
    drops: usize,
    sent: Vec<Vec<u8>>,
    ready: bool,
    drain_ready: bool,
    delay_until_drain: bool,
    fail_open: bool,
    fail_receive: bool,
    output_size: Option<usize>,
}
#[derive(Clone)]
struct Config(Rc<RefCell<Control>>);
impl Default for Config {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(Control {
            ready: true,
            drain_ready: true,
            ..Default::default()
        })))
    }
}
struct Fake {
    config: Config,
    queued: VecDeque<Vec<u8>>,
    draining: bool,
}
impl Engine for Fake {
    type Config = Config;
    type Input = Vec<u8>;
    type Output = Vec<u8>;
    fn open(config: &Config) -> Result<Self> {
        if config.0.borrow().fail_open {
            return Err(Error::unsupported("unavailable codec"));
        }
        config.0.borrow_mut().opens += 1;
        Ok(Self {
            config: config.clone(),
            queued: VecDeque::new(),
            draining: false,
        })
    }
    fn validate(&self, input: &Vec<u8>) -> Result<()> {
        if input.is_empty() {
            Err(Error::invalid("empty input"))
        } else {
            Ok(())
        }
    }
    fn send(&mut self, input: &Vec<u8>) -> Result<bool> {
        let mut c = self.config.0.borrow_mut();
        if !c.ready {
            return Ok(false);
        }
        c.sent.push(input.clone());
        self.queued
            .push_back(c.output_size.map_or_else(|| input.clone(), |n| vec![1; n]));
        Ok(true)
    }
    fn receive(&mut self) -> Result<Receive<Vec<u8>>> {
        let c = self.config.0.borrow();
        if c.fail_receive {
            return Err(Error::invalid("native failure"));
        }
        if c.delay_until_drain && !self.draining {
            return Ok(Receive::Pending);
        }
        Ok(if let Some(value) = self.queued.pop_front() {
            Receive::Output(value)
        } else if self.draining {
            Receive::End
        } else {
            Receive::Pending
        })
    }
    fn drain(&mut self) -> Result<bool> {
        self.draining = self.config.0.borrow().drain_ready;
        Ok(self.draining)
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.config.0.borrow_mut().drops += 1;
    }
}
fn codec(config: &Config, items: usize, bytes: usize) -> Codec<Fake> {
    let mut c = Codec::new(
        Limits {
            max_items: items,
            max_bytes: bytes,
        },
        Limits {
            max_items: 1,
            max_bytes: bytes,
        },
    )
    .unwrap();
    c.configure(config.clone()).unwrap();
    c
}

#[test]
fn full_output_stops_input_consumption_and_rejected_input_is_returned() {
    let config = Config::default();
    let mut c = codec(&config, 2, 6);
    c.try_submit(vec![1; 3]).unwrap();
    c.try_submit(vec![2; 3]).unwrap();
    let rejected = c.try_submit(vec![3]);
    let rejected = rejected.unwrap_err();
    assert_eq!(rejected.error.kind, ErrorKind::WouldBlock);
    assert_eq!(rejected.value, [3]);
    c.pump(100).unwrap();
    assert_eq!(config.0.borrow().sent, [vec![1; 3]]);
    assert_eq!(c.queue_size(), 1);
    assert_eq!(c.next_output(), Some(vec![1; 3]));
    c.pump(100).unwrap();
    assert_eq!(c.next_output(), Some(vec![2; 3]));
    assert_eq!(
        c.try_submit(vec![0; 7]).unwrap_err().error.kind,
        ErrorKind::InvalidArgument
    );
    assert_eq!(
        c.try_submit(vec![]).unwrap_err().error.kind,
        ErrorKind::InvalidArgument
    );
    assert_eq!(c.state(), CodecState::Configured);
}

#[test]
fn full_byte_budget_also_stops_input_before_item_capacity_is_reached() {
    let config = Config::default();
    let mut c = Codec::<Fake>::new(
        Limits {
            max_items: 4,
            max_bytes: 8,
        },
        Limits {
            max_items: 4,
            max_bytes: 4,
        },
    )
    .unwrap();
    c.configure(config.clone()).unwrap();
    c.try_submit(vec![1; 4]).unwrap();
    c.try_submit(vec![2; 4]).unwrap();
    c.pump(100).unwrap();
    assert_eq!(c.output_queue_size(), 1);
    assert_eq!(c.queue_size(), 1);
    assert_eq!(config.0.borrow().sent.len(), 1);
    assert_eq!(c.next_output(), Some(vec![1; 4]));
    c.pump(100).unwrap();
    assert_eq!(c.next_output(), Some(vec![2; 4]));
}

#[test]
fn async_pending_preserves_input_and_drain_can_wait_without_being_eof() {
    let config = Config::default();
    config.0.borrow_mut().ready = false;
    config.0.borrow_mut().drain_ready = false;
    let mut c = codec(&config, 2, 10);
    c.try_submit(vec![1]).unwrap();
    assert_eq!(c.pump(100).unwrap(), 0);
    assert_eq!(c.queue_size(), 1);
    let token = c.begin_flush().unwrap();
    assert!(!c.flush_complete(token).unwrap());
    assert_eq!(
        c.try_submit(vec![2]).unwrap_err().error.kind,
        ErrorKind::WouldBlock
    );
    config.0.borrow_mut().ready = true;
    c.pump(100).unwrap();
    assert_eq!(c.next_output(), Some(vec![1]));
    assert_eq!(c.pump(100).unwrap(), 0);
    assert!(!c.flush_complete(token).unwrap());
    config.0.borrow_mut().drain_ready = true;
    c.pump(100).unwrap();
    assert!(c.flush_complete(token).unwrap());
    c.try_submit(vec![2]).unwrap();
}

#[test]
fn flush_delivers_delayed_frames_before_completion_and_reopens_history() {
    let config = Config::default();
    config.0.borrow_mut().delay_until_drain = true;
    let mut c = codec(&config, 2, 10);
    c.try_submit(vec![1]).unwrap();
    c.try_submit(vec![2]).unwrap();
    c.pump(100).unwrap();
    assert!(c.next_output().is_none());
    assert_eq!(
        c.configure(config.clone()).unwrap_err().kind,
        ErrorKind::WouldBlock
    );
    let token = c.begin_flush().unwrap();
    for value in [1, 2] {
        c.pump(100).unwrap();
        assert!(!c.flush_complete(token).unwrap());
        assert_eq!(c.next_output(), Some(vec![value]));
    }
    c.pump(100).unwrap();
    assert!(c.flush_complete(token).unwrap());
    assert_eq!(config.0.borrow().opens, 2);
    assert_eq!(config.0.borrow().drops, 1);
    let empty = c.begin_flush().unwrap();
    c.pump(100).unwrap();
    assert!(c.flush_complete(empty).unwrap());
    c.configure(config.clone()).unwrap();
}

#[test]
fn reset_cancels_flush_and_releases_history_close_is_terminal() {
    let config = Config::default();
    let mut c = codec(&config, 2, 10);
    c.try_submit(vec![1]).unwrap();
    let token = c.begin_flush().unwrap();
    c.pump(100).unwrap();
    c.reset().unwrap();
    assert_eq!(
        c.flush_complete(token).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert_eq!(config.0.borrow().drops, 1);
    assert_eq!(c.state(), CodecState::Unconfigured);
    assert!(c.next_output().is_none());
    assert_eq!(c.queue_size(), 0);
    c.configure(config.clone()).unwrap();
    c.close();
    c.close();
    assert_eq!(config.0.borrow().drops, 2);
    assert_eq!(c.reset().unwrap_err().kind, ErrorKind::InvalidState);
    assert_eq!(
        c.configure(config).unwrap_err().kind,
        ErrorKind::InvalidState
    );
}

#[test]
fn fatal_output_and_reopen_errors_close_and_cancel_the_session() {
    for oversized in [false, true] {
        let config = Config::default();
        let mut c = codec(&config, 2, 10);
        c.try_submit(vec![1]).unwrap();
        let token = c.begin_flush().unwrap();
        if oversized {
            config.0.borrow_mut().output_size = Some(11);
        } else {
            config.0.borrow_mut().fail_receive = true;
        }
        assert!(c.pump(100).is_err());
        assert_eq!(c.state(), CodecState::Closed);
        assert_eq!(
            c.flush_complete(token).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        assert_eq!(c.queue_size(), 0);
        assert!(c.next_output().is_none());
    }
    let config = Config::default();
    let mut c = codec(&config, 2, 10);
    let token = c.begin_flush().unwrap();
    config.0.borrow_mut().fail_open = true;
    assert!(c.pump(100).is_err());
    assert_eq!(c.state(), CodecState::Closed);
    assert_eq!(
        c.flush_complete(token).unwrap_err().kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn failed_configuration_keeps_the_current_engine_and_support_releases_its_probe() {
    let config = Config::default();
    let bad = Config::default();
    bad.0.borrow_mut().fail_open = true;
    let mut c = codec(&config, 2, 10);
    assert!(c.configure(bad.clone()).is_err());
    c.try_submit(vec![1]).unwrap();
    c.pump(100).unwrap();
    assert_eq!(c.next_output(), Some(vec![1]));
    assert!(!support::<Fake>(bad).unwrap().supported);
    assert!(support::<Fake>(config.clone()).unwrap().supported);
    assert_eq!(config.0.borrow().drops, 1);
}
