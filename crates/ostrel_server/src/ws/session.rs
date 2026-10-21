//! The frames of one WebSocket session after its `Hello` (ARCHITECTURE 5.4, D106, D155).
//!
//! [`Session`] holds no I/O. The transport hands it every received frame
//! ([`Session::receive`]) and does what the returned [`Incoming`] says: send a frame and keep
//! the connection, send a fatal `error` and close, pass a sync message on to the sync engine,
//! or run a call. When a call has its result, the transport asks [`Session::finish`] for the
//! `return` frame.
//!
//! The session remembers the calls it has not answered yet. They are the server side of
//! [`Requests::call_function`]: [`encode_server`] checks the result of a `return` against the
//! result type of the function of its call (arne T175 F3), so a call stays open until its
//! `return` is written.
//!
//! # Rules
//!
//! * A frame that [`decode_client`] refuses gets [`WireError::reply`]: `return` with the call
//!   error for a refused `call` (unknown function `NotFound`, arguments that do not fit
//!   `Invalid`) and the connection stays; a fatal `error` for everything else.
//! * A second `hello` is out of order: fatal `Error Protocol`.
//! * A `call` whose id names a call that is still open is fatal `Error Protocol`: its `return`
//!   could not be told apart. This holds before every other check of the frame, also when
//!   its function is unknown or its arguments do not fit: a refusal `return` for that id would
//!   reach the client as the result of the call that is still running. Ids of closed calls are
//!   not remembered (D155).
//! * A `return` whose result does not fit one frame is written with `CallError` `Limit` instead
//!   (D121, done by [`encode_server`]).
//!
//! ASSUMPTION: the rule for a repeated open call id is not written in ARCHITECTURE 5.4 or
//! SPEC 12.7; D155 makes it a client defect, and every other malformed request is fatal
//! `Error Protocol`, so this one is too.

use std::collections::BTreeMap;
use std::fmt;

use ostrel_core::ids::RowId;
use ostrel_core::value::Value;
use ostrel_sync::protocol::{
    CallError, CallId, ClientMsg, ErrorCode, FnId, ModelId, ProtocolError, ServerMsg, SubId,
};
use ostrel_sync::wire::{Requests, WireError, WireSchema, decode_client, encode_server};

/// The fatal `error` frame with code `Protocol`, written by hand only for the case that the
/// encoder refused an `error` frame, which carries no value and names no model.
const PROTOCOL_ERROR_FRAME: &str = r#"{"code":"Protocol","sub":null,"t":"error","v":0}"#;

/// What the transport does with one received frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// Send this frame and keep the connection.
    Reply(String),
    /// Send this fatal `error` frame and close the connection.
    Close(String),
    /// Run this call ([`super::CallServer::call`]) and then write its `return` with
    /// [`Session::finish`].
    Call(CallJob),
    /// A sync message (`subscribe`, `unsubscribe`, `held`, `ops`) for the sync engine.
    Sync(ClientMsg),
}

/// One call to run for the session.
#[derive(Clone, Debug, PartialEq)]
pub struct CallJob {
    /// The id the client gave the call.
    pub call: CallId,
    /// The principal of the session; it is the same for every call of the session.
    pub principal: Option<RowId>,
    /// The called server fn.
    pub function: FnId,
    /// The arguments, typed by the parameters of the fn.
    pub args: Vec<Value>,
}

/// The calls of a session that wait for their `return`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenCalls(BTreeMap<CallId, FnId>);

impl OpenCalls {
    /// The number of open calls.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when no call waits.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Requests for OpenCalls {
    /// The server serves no subscription through this module, so a `snapshot` is never
    /// written here.
    fn subscription_model(&self, _sub: SubId) -> Option<ModelId> {
        None
    }

    fn call_function(&self, call: CallId) -> Option<FnId> {
        self.0.get(&call).copied()
    }
}

/// One open WebSocket session whose `Hello` was accepted.
pub struct Session {
    principal: Option<RowId>,
    calls: OpenCalls,
}

impl Session {
    /// The session of `principal` (`None` in an app without `auth`), opened by an accepted
    /// `Hello`.
    pub fn new(principal: Option<RowId>) -> Self {
        Session {
            principal,
            calls: OpenCalls::default(),
        }
    }

    /// The principal every call of this session runs as.
    pub fn principal(&self) -> Option<RowId> {
        self.principal
    }

    /// The calls that wait for their `return`.
    pub fn open_calls(&self) -> &OpenCalls {
        &self.calls
    }

    /// Reads one received frame.
    pub fn receive(&mut self, frame: &[u8], schema: &dyn WireSchema) -> Incoming {
        let msg = match decode_client(frame, schema) {
            Ok(msg) => msg,
            Err(error) => return self.refuse(&error, schema),
        };
        match msg {
            ClientMsg::Hello(_) => Incoming::Close(fatal(ErrorCode::Protocol, schema, &self.calls)),
            ClientMsg::Call {
                call,
                function,
                args,
            } => {
                if self.calls.0.contains_key(&call) {
                    return Incoming::Close(fatal(ErrorCode::Protocol, schema, &self.calls));
                }
                self.calls.0.insert(call, function);
                Incoming::Call(CallJob {
                    call,
                    principal: self.principal,
                    function,
                    args,
                })
            }
            other => Incoming::Sync(other),
        }
    }

    /// The `return` frame of `call` with `result`; the call is closed afterwards.
    ///
    /// A result value that does not fit the result type of the fn is a defect of the server
    /// (the VM checks results), reported as `Err`; the call is closed all the same and the
    /// transport closes the connection, because the client could no longer trust it.
    pub fn finish(
        &mut self,
        call: CallId,
        result: Result<Value, CallError>,
        schema: &dyn WireSchema,
    ) -> Result<String, WireError> {
        let frame = encode_server(&ServerMsg::Return { call, result }, schema, &self.calls);
        self.calls.0.remove(&call);
        frame
    }

    fn refuse(&self, error: &WireError, schema: &dyn WireSchema) -> Incoming {
        if let WireError::InvalidCall { call, .. } = error
            && self.calls.0.contains_key(call)
        {
            return Incoming::Close(fatal(ErrorCode::Protocol, schema, &self.calls));
        }
        let reply = error.reply();
        let frame = encode_server(&reply, schema, &self.calls);
        match frame {
            Ok(frame) if !error.closes_connection() => Incoming::Reply(frame),
            Ok(frame) => Incoming::Close(frame),
            Err(_) => Incoming::Close(PROTOCOL_ERROR_FRAME.to_owned()),
        }
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("principal", &self.principal.map(RowId::to_hex))
            .field("open_calls", &self.calls.len())
            .finish()
    }
}

/// A fatal `error` frame with `code`.
fn fatal(code: ErrorCode, schema: &dyn WireSchema, requests: &dyn Requests) -> String {
    let msg = ServerMsg::Error(ProtocolError { sub: None, code });
    encode_server(&msg, schema, requests).unwrap_or_else(|_| PROTOCOL_ERROR_FRAME.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoSchema;

    impl WireSchema for NoSchema {
        fn model_id(&self, _: &str) -> Option<ModelId> {
            None
        }
        fn model_name(&self, _: ModelId) -> Option<&str> {
            None
        }
        fn field_id(&self, _: ModelId, _: &str) -> Option<ostrel_sync::protocol::FieldId> {
            None
        }
        fn field_name(&self, _: ModelId, _: ostrel_sync::protocol::FieldId) -> Option<&str> {
            None
        }
        fn field_ty(
            &self,
            _: ModelId,
            _: ostrel_sync::protocol::FieldId,
        ) -> Option<&ostrel_sync::wire::WireTy> {
            None
        }
        fn function_id(&self, _: &str) -> Option<FnId> {
            None
        }
        fn function_name(&self, _: FnId) -> Option<&str> {
            None
        }
        fn function_params(&self, _: FnId) -> Option<&[ostrel_sync::wire::WireTy]> {
            None
        }
        fn function_result(&self, _: FnId) -> Option<&ostrel_sync::wire::WireTy> {
            None
        }
    }

    #[test]
    fn the_hand_written_protocol_error_is_what_the_encoder_writes() {
        assert_eq!(
            fatal(ErrorCode::Protocol, &NoSchema, &OpenCalls::default()),
            PROTOCOL_ERROR_FRAME
        );
    }

    #[test]
    fn the_session_debug_output_names_no_call_content() {
        let session = Session::new(None);
        assert_eq!(
            format!("{session:?}"),
            "Session { principal: None, open_calls: 0 }"
        );
    }
}
