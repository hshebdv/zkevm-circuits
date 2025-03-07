//! Execution step related module.

use std::marker::PhantomData;

use crate::{
    circuit_input_builder::CallContext, error::ExecError, exec_trace::OperationRef,
    operation::RWCounter, precompile::PrecompileCalls,
};
use eth_types::{
    evm_types::{Gas, GasCost, OpcodeId, ProgramCounter},
    GethExecStep, Word, H256,
};
use gadgets::impl_expr;
use halo2_proofs::plonk::Expression;
use strum::IntoEnumIterator;

/// An execution step of the EVM.
#[derive(Clone, Debug)]
pub struct ExecStep {
    /// Execution state
    pub exec_state: ExecState,
    /// Program Counter
    pub pc: ProgramCounter,
    /// Stack size
    pub stack_size: usize,
    /// Memory size
    pub memory_size: usize,
    /// Gas left
    pub gas_left: Gas,
    /// Gas cost of the step.  If the error is OutOfGas caused by a "gas uint64
    /// overflow", this value will **not** be the actual Gas cost of the
    /// step.
    pub gas_cost: GasCost,
    /// Accumulated gas refund
    pub gas_refund: Gas,
    /// Call index within the Transaction.
    pub call_index: usize,
    /// The global counter when this step was executed.
    pub rwc: RWCounter,
    /// Reversible Write Counter.  Counter of write operations in the call that
    /// will need to be undone in case of a revert.  Value at the beginning of
    /// the step.
    pub reversible_write_counter: usize,
    /// Number of reversible write operations done by this step.
    pub reversible_write_counter_delta: usize,
    /// Log index when this step was executed.
    pub log_id: usize,
    /// The list of references to Operations in the container
    pub bus_mapping_instance: Vec<OperationRef>,
    /// Number of rw operations performed via a copy event in this step.
    pub copy_rw_counter_delta: u64,
    /// Error generated by this step
    pub error: Option<ExecError>,
}

impl ExecStep {
    /// Create a new Self from a `GethExecStep`.
    pub fn new(
        step: &GethExecStep,
        call_ctx: &CallContext,
        rwc: RWCounter,
        reversible_write_counter: usize,
        log_id: usize,
    ) -> Self {
        ExecStep {
            exec_state: ExecState::Op(step.op),
            pc: step.pc,
            stack_size: step.stack.0.len(),
            memory_size: call_ctx.memory.len(),
            gas_left: step.gas,
            gas_cost: step.gas_cost,
            gas_refund: step.refund,
            call_index: call_ctx.index,
            rwc,
            reversible_write_counter,
            reversible_write_counter_delta: 0,
            log_id,
            bus_mapping_instance: Vec::new(),
            copy_rw_counter_delta: 0,
            error: None,
        }
    }

    /// Returns `true` if `error` is oog and stack related..
    pub fn oog_or_stack_error(&self) -> bool {
        matches!(
            self.error,
            Some(ExecError::OutOfGas(_) | ExecError::StackOverflow | ExecError::StackUnderflow)
        )
    }

    /// Returns `true` if this is an execution step of Precompile.
    pub fn is_precompiled(&self) -> bool {
        matches!(self.exec_state, ExecState::Precompile(_))
    }
}

impl Default for ExecStep {
    fn default() -> Self {
        Self {
            exec_state: ExecState::Op(OpcodeId::INVALID(0)),
            pc: ProgramCounter(0),
            stack_size: 0,
            memory_size: 0,
            gas_left: Gas(0),
            gas_cost: GasCost(0),
            gas_refund: Gas(0),
            call_index: 0,
            rwc: RWCounter(0),
            reversible_write_counter: 0,
            reversible_write_counter_delta: 0,
            log_id: 0,
            bus_mapping_instance: Vec::new(),
            copy_rw_counter_delta: 0,
            error: None,
        }
    }
}

/// Execution state
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecState {
    /// EVM Opcode ID
    Op(OpcodeId),
    /// Precompile call
    Precompile(PrecompileCalls),
    /// Virtual step Begin Tx
    BeginTx,
    /// Virtual step End Tx
    EndTx,
    /// Virtual step End Block
    EndBlock,
}

impl ExecState {
    /// Returns `true` if `ExecState` is an opcode and the opcode is a `PUSHn`.
    pub fn is_push(&self) -> bool {
        if let ExecState::Op(op) = self {
            op.is_push()
        } else {
            false
        }
    }

    /// Returns `true` if `ExecState` is an opcode and the opcode is a `DUPn`.
    pub fn is_dup(&self) -> bool {
        if let ExecState::Op(op) = self {
            op.is_dup()
        } else {
            false
        }
    }

    /// Returns `true` if `ExecState` is an opcode and the opcode is a `SWAPn`.
    pub fn is_swap(&self) -> bool {
        if let ExecState::Op(op) = self {
            op.is_swap()
        } else {
            false
        }
    }

    /// Returns `true` if `ExecState` is an opcode and the opcode is a `Logn`.
    pub fn is_log(&self) -> bool {
        if let ExecState::Op(op) = self {
            op.is_log()
        } else {
            false
        }
    }
}

/// Defines the various source/destination types for a copy event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyDataType {
    /// When we need to pad the Copy rows of the circuit up to a certain maximum
    /// with rows that are not "useful".
    Padding,
    /// When the source for the copy event is the bytecode table.
    Bytecode,
    /// When the source/destination for the copy event is memory.
    Memory,
    /// When the source for the copy event is tx's calldata.
    TxCalldata,
    /// When the destination for the copy event is tx's log.
    TxLog,
    /// When the destination rows are not directly for copying but for a special
    /// scenario where we wish to accumulate the value (RLC) over all rows.
    /// This is used for Copy Lookup from SHA3 opcode verification.
    RlcAcc,
    /// When the source of the copy is a call to a precompiled contract.
    Precompile(PrecompileCalls),
}
impl CopyDataType {
    /// Get variants that represent a precompile call.
    pub fn precompile_types() -> Vec<Self> {
        PrecompileCalls::iter().map(Self::Precompile).collect()
    }
}
const NUM_COPY_DATA_TYPES: usize = 15usize;
pub struct CopyDataTypeIter {
    idx: usize,
    back_idx: usize,
    marker: PhantomData<()>,
}
impl CopyDataTypeIter {
    fn get(&self, idx: usize) -> Option<CopyDataType> {
        match idx {
            0usize => Some(CopyDataType::Padding),
            1usize => Some(CopyDataType::Bytecode),
            2usize => Some(CopyDataType::Memory),
            3usize => Some(CopyDataType::TxCalldata),
            4usize => Some(CopyDataType::TxLog),
            5usize => Some(CopyDataType::RlcAcc),
            6usize => Some(CopyDataType::Precompile(PrecompileCalls::ECRecover)),
            7usize => Some(CopyDataType::Precompile(PrecompileCalls::Sha256)),
            8usize => Some(CopyDataType::Precompile(PrecompileCalls::Ripemd160)),
            9usize => Some(CopyDataType::Precompile(PrecompileCalls::Identity)),
            10usize => Some(CopyDataType::Precompile(PrecompileCalls::Modexp)),
            11usize => Some(CopyDataType::Precompile(PrecompileCalls::Bn128Add)),
            12usize => Some(CopyDataType::Precompile(PrecompileCalls::Bn128Mul)),
            13usize => Some(CopyDataType::Precompile(PrecompileCalls::Bn128Pairing)),
            14usize => Some(CopyDataType::Precompile(PrecompileCalls::Blake2F)),
            _ => None,
        }
    }
}
impl strum::IntoEnumIterator for CopyDataType {
    type Iterator = CopyDataTypeIter;
    fn iter() -> CopyDataTypeIter {
        CopyDataTypeIter {
            idx: 0,
            back_idx: 0,
            marker: PhantomData,
        }
    }
}
impl Iterator for CopyDataTypeIter {
    type Item = CopyDataType;
    fn next(&mut self) -> Option<<Self as Iterator>::Item> {
        #[allow(clippy::iter_nth_zero)]
        self.nth(0)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let t = if self.idx + self.back_idx >= NUM_COPY_DATA_TYPES {
            0
        } else {
            NUM_COPY_DATA_TYPES - self.idx - self.back_idx
        };
        (t, Some(t))
    }
    fn nth(&mut self, n: usize) -> Option<<Self as Iterator>::Item> {
        let idx = self.idx + n + 1;
        if idx + self.back_idx > NUM_COPY_DATA_TYPES {
            self.idx = NUM_COPY_DATA_TYPES;
            None
        } else {
            self.idx = idx;
            self.get(idx - 1)
        }
    }
}
impl ExactSizeIterator for CopyDataTypeIter {
    fn len(&self) -> usize {
        self.size_hint().0
    }
}
impl DoubleEndedIterator for CopyDataTypeIter {
    fn next_back(&mut self) -> Option<<Self as Iterator>::Item> {
        let back_idx = self.back_idx + 1;
        if self.idx + back_idx > NUM_COPY_DATA_TYPES {
            self.back_idx = NUM_COPY_DATA_TYPES;
            None
        } else {
            self.back_idx = back_idx;
            self.get(NUM_COPY_DATA_TYPES - self.back_idx)
        }
    }
}

impl From<CopyDataType> for usize {
    fn from(t: CopyDataType) -> Self {
        match t {
            CopyDataType::Padding => 0,
            CopyDataType::Bytecode => 1,
            CopyDataType::Memory => 2,
            CopyDataType::TxCalldata => 3,
            CopyDataType::TxLog => 4,
            CopyDataType::RlcAcc => 5,
            CopyDataType::Precompile(prec_call) => 5 + usize::from(prec_call),
        }
    }
}

impl From<&CopyDataType> for u64 {
    fn from(t: &CopyDataType) -> Self {
        match t {
            CopyDataType::Padding => 0,
            CopyDataType::Bytecode => 1,
            CopyDataType::Memory => 2,
            CopyDataType::TxCalldata => 3,
            CopyDataType::TxLog => 4,
            CopyDataType::RlcAcc => 5,
            CopyDataType::Precompile(prec_call) => 5 + u64::from(*prec_call),
        }
    }
}

impl Default for CopyDataType {
    fn default() -> Self {
        Self::Memory
    }
}

impl_expr!(CopyDataType, u64::from);

/// Defines a single copy step in a copy event. This type is unified over the
/// source/destination row in the copy table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyStep {
    /// Byte value copied in this step.
    pub value: u8,
    /// Optional field which is enabled only for the source being `bytecode`,
    /// and represents whether or not the byte is an opcode.
    pub is_code: Option<bool>,
}

/// Defines an enum type that can hold either a number or a hash value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberOrHash {
    /// Variant to indicate a number value.
    Number(usize),
    /// Variant to indicate a 256-bits hash value.
    Hash(H256),
}

/// Defines a copy event associated with EVM opcodes such as CALLDATACOPY,
/// CODECOPY, CREATE, etc. More information:
/// <https://github.com/privacy-scaling-explorations/zkevm-specs/blob/master/specs/copy-proof.md>.
#[derive(Clone, Debug)]
pub struct CopyEvent {
    /// Represents the start address at the source of the copy event.
    pub src_addr: u64,
    /// Represents the end address at the source of the copy event.
    pub src_addr_end: u64,
    /// Represents the source type.
    pub src_type: CopyDataType,
    /// Represents the relevant ID for source.
    pub src_id: NumberOrHash,
    /// Represents the start address at the destination of the copy event.
    pub dst_addr: u64,
    /// Represents the destination type.
    pub dst_type: CopyDataType,
    /// Represents the relevant ID for destination.
    pub dst_id: NumberOrHash,
    /// An optional field to hold the log ID in case of the destination being
    /// TxLog.
    pub log_id: Option<u64>,
    /// Value of rw counter at start of this copy event
    pub rw_counter_start: RWCounter,
    /// Represents the list of (bytes, is_code) copied during this copy event
    pub bytes: Vec<(u8, bool)>,
}

impl CopyEvent {
    /// rw counter at step index
    pub fn rw_counter(&self, step_index: usize) -> u64 {
        u64::try_from(self.rw_counter_start.0).unwrap() + self.rw_counter_increase(step_index)
    }

    /// rw counter increase left at step index
    pub fn rw_counter_increase_left(&self, step_index: usize) -> u64 {
        self.rw_counter(self.bytes.len() * 2) - self.rw_counter(step_index)
    }

    /// Number of rw operations performed by this copy event
    pub fn rw_counter_delta(&self) -> u64 {
        self.rw_counter_increase(self.bytes.len() * 2)
    }

    // increase in rw counter from the start of the copy event to step index
    fn rw_counter_increase(&self, step_index: usize) -> u64 {
        let source_rw_increase = match self.src_type {
            CopyDataType::Bytecode | CopyDataType::TxCalldata | CopyDataType::Precompile(_) => 0,
            CopyDataType::Memory => std::cmp::min(
                u64::try_from(step_index + 1).unwrap() / 2,
                self.src_addr_end
                    .checked_sub(self.src_addr)
                    .unwrap_or_default(),
            ),
            CopyDataType::RlcAcc | CopyDataType::TxLog | CopyDataType::Padding => unreachable!(),
        };
        let destination_rw_increase = match self.dst_type {
            CopyDataType::RlcAcc | CopyDataType::Bytecode | CopyDataType::Precompile(_) => 0,
            CopyDataType::TxLog | CopyDataType::Memory => u64::try_from(step_index).unwrap() / 2,
            CopyDataType::TxCalldata | CopyDataType::Padding => {
                unreachable!()
            }
        };
        source_rw_increase + destination_rw_increase
    }
}

/// Intermediary multiplication step, representing `a * b == d (mod 2^256)`
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpStep {
    /// First multiplicand.
    pub a: Word,
    /// Second multiplicand.
    pub b: Word,
    /// Multiplication result.
    pub d: Word,
}

impl From<(Word, Word, Word)> for ExpStep {
    fn from(values: (Word, Word, Word)) -> Self {
        Self {
            a: values.0,
            b: values.1,
            d: values.2,
        }
    }
}

/// Event representating an exponentiation `a ^ b == d (mod 2^256)`.
#[derive(Clone, Debug)]
pub struct ExpEvent {
    /// Identifier for the exponentiation trace.
    pub identifier: usize,
    /// Base `a` for the exponentiation.
    pub base: Word,
    /// Exponent `b` for the exponentiation.
    pub exponent: Word,
    /// Exponentiation result.
    pub exponentiation: Word,
    /// Intermediate multiplication results.
    pub steps: Vec<ExpStep>,
}

impl Default for ExpEvent {
    fn default() -> Self {
        Self {
            identifier: 0,
            base: 2.into(),
            exponent: 2.into(),
            exponentiation: 4.into(),
            steps: vec![ExpStep {
                a: 2.into(),
                b: 2.into(),
                d: 4.into(),
            }],
        }
    }
}
