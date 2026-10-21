//! The turn state machine with compile-time legal edges.
//!
//! A [`Turn`] carries its phase as a type. [`Turn::advance`] is only callable when an
//! [`Edge`] between the two phases exists, so an illegal transition (for example from
//! tool execution straight to completion) does not compile. The persisted
//! [`TurnState`] is still checked with `permits` as defense in depth.
//!
//! ```
//! use layeragent_core::fsm::*;
//! fn legal<From: Edge<To>, To>() {}
//! legal::<PreflightPhase, ContextPhase>();
//! legal::<ContextPhase, InferencePhase>();
//! legal::<InferencePhase, ToolsPhase>();
//! legal::<InferencePhase, FinalPhase>();
//! legal::<ToolsPhase, ContextPhase>();
//! legal::<FinalPhase, DonePhase>();
//! ```
//!
//! Skipping inference, resuming a finished turn or finishing from tool execution are
//! rejected by the compiler:
//!
//! ```compile_fail
//! use layeragent_core::fsm::*;
//! fn legal<From: Edge<To>, To>() {}
//! legal::<ToolsPhase, FinalPhase>();
//! ```
//!
//! ```compile_fail
//! use layeragent_core::fsm::*;
//! fn legal<From: Edge<To>, To>() {}
//! legal::<DonePhase, ContextPhase>();
//! ```
//!
//! ```compile_fail
//! use layeragent_core::fsm::*;
//! fn legal<From: Edge<To>, To>() {}
//! legal::<ContextPhase, FinalPhase>();
//! ```
use anyhow::{Result, ensure};
use layeragent_protocol::TurnState;
use layeragent_storage::Storage;
use std::marker::PhantomData;

pub struct PreflightPhase;
pub struct ContextPhase;
pub struct InferencePhase;
pub struct ToolsPhase;
pub struct FinalPhase;
pub struct DonePhase;

mod sealed {
    pub trait Sealed<To> {}
}

/// A legal transition from `Self` to `To`. Implemented only in this module.
pub trait Edge<To>: sealed::Sealed<To> {
    const TO: TurnState;
}

macro_rules! edge {
    ($from:ty => $to:ty, $state:expr) => {
        impl sealed::Sealed<$to> for $from {}
        impl Edge<$to> for $from {
            const TO: TurnState = $state;
        }
    };
}
edge!(PreflightPhase => ContextPhase, TurnState::Context);
edge!(ContextPhase => InferencePhase, TurnState::Inference);
edge!(InferencePhase => ToolsPhase, TurnState::Tools);
edge!(InferencePhase => FinalPhase, TurnState::Finalizing);
edge!(ToolsPhase => ContextPhase, TurnState::Context);
edge!(FinalPhase => DonePhase, TurnState::Completed);

pub struct Turn<S> {
    pub id: String,
    state: TurnState,
    marker: PhantomData<S>,
}

pub fn state_name(state: &TurnState) -> &'static str {
    match state {
        TurnState::Preflight => "preflight",
        TurnState::Context => "context",
        TurnState::Inference => "inference",
        TurnState::Tools => "tools",
        TurnState::AwaitingApproval => "awaiting_approval",
        TurnState::Finalizing => "finalizing",
        TurnState::Completed => "completed",
        TurnState::Failed => "failed",
        TurnState::Interrupted => "interrupted",
    }
}

impl Turn<PreflightPhase> {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            state: TurnState::Preflight,
            marker: PhantomData,
        }
    }
}

impl<S> Turn<S> {
    pub async fn advance<N>(self, store: &Storage) -> Result<Turn<N>>
    where
        S: Edge<N>,
    {
        let to = <S as Edge<N>>::TO;
        ensure!(self.state.permits(&to), "invalid turn transition");
        store.turn_state(&self.id, state_name(&to)).await?;
        Ok(Turn {
            id: self.id,
            state: to,
            marker: PhantomData,
        })
    }
}

impl Turn<ToolsPhase> {
    /// Tool execution pauses for a human decision and resumes in the same phase.
    pub async fn awaiting_approval(&mut self, store: &Storage) -> Result<()> {
        ensure!(
            self.state.permits(&TurnState::AwaitingApproval),
            "invalid turn transition"
        );
        store.turn_state(&self.id, "awaiting_approval").await?;
        self.state = TurnState::AwaitingApproval;
        Ok(())
    }
    pub async fn resume_tools(&mut self, store: &Storage) -> Result<()> {
        ensure!(
            self.state.permits(&TurnState::Tools),
            "invalid turn transition"
        );
        store.turn_state(&self.id, "tools").await?;
        self.state = TurnState::Tools;
        Ok(())
    }
}
