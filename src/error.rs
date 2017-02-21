use bincode;
use mio::timer::TimerError;
use std::io;

quick_error! {
    /// Nat-traversal's universal error type.
    #[derive(Debug)]
    pub enum NatError {
        /// Io Error
        Io(e: io::Error) {
            description(e.description())
            display("{}", e)
            cause(e)
            from()
        }
        /// Timer error
        Timer(e: TimerError) {
            description(e.description())
            display("{}", e)
            cause(e)
            from()
        }
        /// Serialization errors
        Serialisation(e: bincode::Error) {
            description(e.description())
            display("{}", e)
            cause(e)
            from()
        }

        // =======================================

        /// Rendezvous with server failed for both Tcp and Udp - could not obtain our external
        /// address
        RendezvousFailed {
            description("Udp Rendezvous with server failed for both Tcp and Udp - could not obtain \
                        our external address")
        }
        /// Udp Rendezvous with server failed - could not obtain our external address
        UdpRendezvousFailed {
            description("Udp Rendezvous with server failed - could not obtain our external address")
        }

        // =======================================

        /// Booting up Hole Punch Mediator failed
        HolePunchMediatorFailedToStart {
            description("Booting Hole Punch Mediator failed")
        }
        /// Booting up Udp Hole Punch Mediator failed
        UdpHolePunchMediatorFailedToStart {
            description("Booting Udp Hole Punch Mediator Failed")
        }
        /// Booting up Udp Rendezvous Server failed
        UdpRendezvousServerStartFailed {
            description("Booting Udp Rendezvous Server Failed")
        }

        // =======================================

        /// Hole punch failed
        HolePunchFailed {
            description("Hole punch failed")
        }
        /// Udp Hole punch failed
        UdpHolePunchFailed {
            description("Udp Hole punch failed")
        }

        // =======================================

        /// Timer ID is invalid
        InvalidTimerId {
            description("Timer ID is invalid")
        }
        /// Invalid state - the state may already be active or is an operation is not supposed to
        /// be permitted for this state
        InvalidState {
            description("Invalid state - the state may already be active or is an operation is not \
                        supposed to be permitted for this state")
        }
        /// Socket is not available
        UnregisteredSocket {
            description("Socket is not available")
        }
        /// Unknown error
        Unknown {
            description("Unknown Error in Nat Traversal")
        }
    }
}