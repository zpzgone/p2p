use {Interface, NatError, NatMsg, NatState, NatTimer};
use config::{HOLE_PUNCH_TIMEOUT_SEC, RENDEZVOUS_TIMEOUT_SEC};
use mio::{Poll, Token};
use mio::channel::Sender;
use mio::tcp::TcpStream;
use mio::timer::Timeout;
use mio::udp::UdpSocket;
use std::any::Any;
use std::cell::RefCell;
use std::fmt::{self, Debug, Formatter};
use std::mem;
use std::net::SocketAddr;
use std::rc::{Rc, Weak};
use std::time::Duration;
use tcp::TcpHolePunchMediator;
use udp::UdpHolePunchMediator;

pub type GetInfo = Box<FnMut(&mut Interface, &Poll, ::Res<(Handle, RendezvousInfo)>)>;
pub type HolePunchFinsih = Box<FnMut(&mut Interface, &Poll, ::Res<HolePunchInfo>) + Send + 'static>;

#[derive(Debug, Serialize, Deserialize)]
pub struct RendezvousInfo {
    pub udp: Vec<SocketAddr>,
    pub tcp: Vec<SocketAddr>,
}
impl Default for RendezvousInfo {
    fn default() -> Self {
        RendezvousInfo {
            udp: vec![],
            tcp: vec![],
        }
    }
}

#[derive(Debug)]
pub struct HolePunchInfo {
    pub tcp: Option<(TcpStream, Token)>,
    pub udp: Option<(UdpSocket, Token)>,
}
impl Default for HolePunchInfo {
    fn default() -> Self {
        HolePunchInfo {
            tcp: None,
            udp: None,
        }
    }
}

const TIMER_ID: u8 = 0;

enum State {
    None,
    Rendezvous {
        info: RendezvousInfo,
        timeout: Timeout,
        f: GetInfo,
    },
    ReadyToHolePunch,
    HolePunching {
        info: HolePunchInfo,
        timeout: Timeout,
        f: HolePunchFinsih,
    },
}
impl Debug for State {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match *self {
            State::None => write!(f, "State::None"),
            State::Rendezvous { .. } => write!(f, "State::Rendezvous"),
            State::ReadyToHolePunch => write!(f, "State::ReadyToHolePunch"),
            State::HolePunching { .. } => write!(f, "State::HolePunching"),
        }
    }
}


pub struct HolePunchMediator {
    token: Token,
    state: State,
    udp_child: Option<Rc<RefCell<UdpHolePunchMediator>>>,
    tcp_child: Option<Rc<RefCell<TcpHolePunchMediator>>>,
    self_weak: Weak<RefCell<HolePunchMediator>>,
}

impl HolePunchMediator {
    pub fn start(ifc: &mut Interface, poll: &Poll, f: GetInfo) -> ::Res<()> {
        let token = ifc.new_token();
        let dur = ifc.config().rendezvous_timeout_sec.unwrap_or(RENDEZVOUS_TIMEOUT_SEC);
        let timeout = ifc.set_timeout(Duration::from_secs(dur), NatTimer::new(token, TIMER_ID))?;

        let mediator = Rc::new(RefCell::new(HolePunchMediator {
            token: token,
            state: State::None,
            udp_child: None,
            tcp_child: None,
            self_weak: Weak::new(),
        }));
        let weak = Rc::downgrade(&mediator);
        mediator.borrow_mut().self_weak = weak.clone();

        let handler = move |ifc: &mut Interface, poll: &Poll, res| if let Some(mediator) =
            weak.upgrade() {
            mediator.borrow_mut().handle_udp_rendezvous(ifc, poll, res);
        };

        let udp_child = match UdpHolePunchMediator::start(ifc, poll, Box::new(handler)) {
            Ok(child) => Some(child),
            Err(e) => {
                debug!("Udp Hole Punch Mediator failed to initialise: {:?}", e);
                None
            }
        };

        let tcp_child = None; // TODO Put TCP logic here

        if udp_child.is_none() && tcp_child.is_none() {
            Err(NatError::RendezvousFailed)
        } else {
            {
                let mut m = mediator.borrow_mut();
                m.state = State::Rendezvous {
                    info: Default::default(),
                    timeout: timeout,
                    f: f,
                };
                m.udp_child = udp_child;
                m.tcp_child = tcp_child;
            }

            if let Err((nat_state, e)) = ifc.insert_state(token, mediator) {
                // TODO Handle properly
                error!("To be handled properly: {}", e);
                nat_state.borrow_mut().terminate(ifc, poll);
                return Err(NatError::HolePunchMediatorFailedToStart);
            }

            Ok(())
        }
    }

    fn handle_udp_rendezvous(&mut self,
                             ifc: &mut Interface,
                             poll: &Poll,
                             res: ::Res<Vec<SocketAddr>>) {
        let r = match self.state {
            State::Rendezvous { ref mut info, ref mut f, ref timeout } => {
                if let Ok(ext_addrs) = res {
                    // We assume that udp_child does not return an empty list here - rather it
                    // should error out on such case (i.e. call us with an error)
                    info.udp = ext_addrs;
                } else {
                    self.udp_child = None;
                }
                if self.tcp_child.is_none() || !info.tcp.is_empty() {
                    if self.udp_child.is_none() && self.tcp_child.is_none() {
                        f(ifc, poll, Err(NatError::RendezvousFailed));
                        Err(NatError::RendezvousFailed)
                    } else {
                        let _ = ifc.cancel_timeout(timeout);
                        let info = mem::replace(info, Default::default());
                        let handle = Handle {
                            token: self.token,
                            tx: ifc.sender().clone(),
                        };
                        f(ifc, poll, Ok((handle, info)));
                        Ok(true)
                    }
                } else {
                    Ok(false)
                }
            }
            ref x => {
                warn!("Logic Error in state book-keeping - Pls report this as a bug. Expected \
                       state: State::Rendezvous ;; Found: {:?}",
                      x);
                Err(NatError::InvalidState)
            }
        };

        match r {
            Ok(true) => self.state = State::ReadyToHolePunch,
            Ok(false) => (),
            Err(e @ NatError::RendezvousFailed) => {
                // This is reached only if children is empty. So no chance of borrow violation for
                // children in terminate()
                debug!("Terminating due to: {:?}", e);
                self.terminate(ifc, poll);
            }
            // Don't call terminate as that can lead to child being borrowed twice
            Err(e) => debug!("Ignoring error in handle hole-punch: {:?}", e),
        }
    }

    fn punch_hole(&mut self,
                  ifc: &mut Interface,
                  poll: &Poll,
                  peers: RendezvousInfo,
                  mut f: HolePunchFinsih) {
        match self.state {
            State::ReadyToHolePunch => (),
            ref x => {
                debug!("Improper state for this operation: {:?}", x);
                return f(ifc, poll, Err(NatError::HolePunchFailed));
            }
        };

        let dur = ifc.config().hole_punch_timeout_sec.unwrap_or(HOLE_PUNCH_TIMEOUT_SEC);
        let timeout = match ifc.set_timeout(Duration::from_secs(dur),
                                            NatTimer::new(self.token, TIMER_ID)) {
            Ok(t) => t,
            Err(e) => {
                debug!("Terminating punch hole due to error in timer: {:?}", e);
                return self.terminate(ifc, poll);
            }
        };

        if let Some(udp_child) = self.udp_child.as_ref().cloned() {
            let weak = self.self_weak.clone();
            let handler = move |ifc: &mut Interface, poll: &Poll, res| if let Some(mediator) =
                weak.upgrade() {
                mediator.borrow_mut().handle_udp_hole_punch(ifc, poll, res);
            };
            if let Err(e) = udp_child.borrow_mut()
                .punch_hole(ifc, poll, peers.udp, Box::new(handler)) {
                debug!("Udp punch hole failed to start: {:?}", e);
                self.udp_child = None;
            }
        }

        if self.udp_child.is_none() && self.tcp_child.is_none() {
            debug!("Failure: Not even one valid child even managed to start hole punching");
            self.terminate(ifc, poll);
            return f(ifc, poll, Err(NatError::HolePunchFailed));
        }

        self.state = State::HolePunching {
            info: Default::default(),
            timeout: timeout,
            f: f,
        };
    }

    fn handle_udp_hole_punch(&mut self,
                             ifc: &mut Interface,
                             poll: &Poll,
                             res: ::Res<(UdpSocket, Token)>) {
        let r = match self.state {
            State::HolePunching { ref mut info, ref mut f, .. } => {
                self.udp_child = None;
                if let Ok(sock) = res {
                    info.udp = Some(sock);
                }
                if self.tcp_child.is_none() && self.udp_child.is_none() {
                    if info.tcp.is_none() && info.udp.is_none() {
                        f(ifc, poll, Err(NatError::HolePunchFailed));
                        Err(NatError::HolePunchFailed)
                    } else {
                        let info = mem::replace(info, Default::default());
                        f(ifc, poll, Ok(info));
                        Ok(true)
                    }
                } else {
                    Ok(false)
                }
            }
            ref x => {
                warn!("Logic Error in state book-keeping - Pls report this as a bug. Expected \
                       state: State::HolePunching ;; Found: {:?}",
                      x);
                Err(NatError::InvalidState)
            }
        };

        match r {
            Ok(true) => self.terminate(ifc, poll),
            Ok(false) => (),
            Err(e @ NatError::HolePunchFailed) => {
                // This is reached only if children is empty. So no chance of borrow violation for
                // children in terminate()
                debug!("Terminating due to: {:?}", e);
                self.terminate(ifc, poll);
            }
            // Don't call terminate as that can lead to child being borrowed twice
            Err(e) => debug!("Ignoring error in handle udp-hole-punch: {:?}", e),
        }
    }
}

impl NatState for HolePunchMediator {
    fn timeout(&mut self, ifc: &mut Interface, poll: &Poll, timer_id: u8) {
        if timer_id != TIMER_ID {
            debug!("Invalid Timer ID: {}", timer_id);
        }

        let terminate = match self.state {
            State::Rendezvous { .. } => {
                if let Some(udp_child) = self.udp_child.as_ref().cloned() {
                    let r = udp_child.borrow_mut().rendezvous_timeout(ifc, poll);
                    self.handle_udp_rendezvous(ifc, poll, r);
                }
                if let Some(_tcp_child) = self.tcp_child.as_ref().cloned() {
                    // let r = tcp_child.borrow_mut().rendezvous_timeout(ifc, poll);
                    // self.handle_tcp_rendezvous(ifc, poll, r);
                }

                false
            }
            State::HolePunching { ref mut info, ref mut f, .. } => {
                if info.tcp.is_none() && info.udp.is_none() {
                    f(ifc, poll, Err(NatError::HolePunchFailed));
                } else {
                    let info = mem::replace(info, Default::default());
                    f(ifc, poll, Ok(info));
                }

                true
            }
            ref x => {
                warn!("Logic error, report bug: terminating due to invalid state for a timeout: \
                       {:?}",
                      x);
                true
            }
        };

        if terminate {
            self.terminate(ifc, poll);
        }
    }

    fn terminate(&mut self, ifc: &mut Interface, poll: &Poll) {
        let _ = ifc.remove_state(self.token);
        match self.state {
            State::Rendezvous { ref timeout, .. } |
            State::HolePunching { ref timeout, .. } => {
                let _ = ifc.cancel_timeout(timeout);
            }
            _ => (),
        }
        if let Some(udp_child) = self.udp_child.take() {
            udp_child.borrow_mut().terminate(ifc, poll);
        }
        if let Some(tcp_child) = self.tcp_child.take() {
            tcp_child.borrow_mut().terminate(ifc, poll);
        }
    }

    fn as_any(&mut self) -> &mut Any {
        self
    }
}

pub struct Handle {
    token: Token,
    tx: Sender<NatMsg>,
}

impl Handle {
    pub fn fire_hole_punch(self, peers: RendezvousInfo, f: HolePunchFinsih) {
        let token = self.token;
        if let Err(e) = self.tx.send(NatMsg::new(move |ifc, poll| {
            Handle::start_hole_punch(ifc, poll, token, peers, f)
        })) {
            debug!("Could not fire hole punch request: {:?}", e);
        } else {
            mem::forget(self);
        }
    }

    pub fn start_hole_punch(ifc: &mut Interface,
                            poll: &Poll,
                            hole_punch_mediator: Token,
                            peers: RendezvousInfo,
                            mut f: HolePunchFinsih) {
        if let Some(nat_state) = ifc.state(hole_punch_mediator) {
            let mut state = nat_state.borrow_mut();
            let mediator = match state.as_any().downcast_mut::<HolePunchMediator>() {
                Some(m) => m,
                None => {
                    debug!("Token has some other state mapped, not HolePunchMediator");
                    return f(ifc, poll, Err(NatError::InvalidState));
                }
            };
            mediator.punch_hole(ifc, poll, peers, f);

        }
    }

    pub fn mediator_token(self) -> Token {
        let token = self.token;
        mem::forget(self);
        token
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let token = self.token;
        let _ = self.tx
            .send(NatMsg::new(move |ifc, poll| if let Some(nat_state) = ifc.state(token) {
                nat_state.borrow_mut().terminate(ifc, poll);
            }));
    }
}