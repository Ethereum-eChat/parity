// Copyright 2015-2017 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

//! Wasm Interpreter

extern crate byteorder;
extern crate ethcore_logger;
extern crate ethereum_types;
#[macro_use] extern crate log;
extern crate libc;
extern crate parity_wasm;
extern crate vm;
extern crate wasm_utils;
extern crate wasmi;

mod runtime;
// mod ptr;
// mod result;
// #[cfg(test)]
// mod tests;
mod env;
// mod panic_payload;

use parity_wasm::elements;

use vm::{GasLeft, ReturnData, ActionParams};
use wasmi::Error as InterpreterError;

use runtime::{Runtime, RuntimeContext};

const DEFAULT_RESULT_BUFFER: usize = 1024;

/// Wrapped interpreter error
#[derive(Debug)]
pub struct Error(InterpreterError);

impl From<InterpreterError> for Error {
	fn from(e: InterpreterError) -> Self {
		Error(e)
	}
}

impl From<Error> for vm::Error {
	fn from(e: Error) -> Self {
		vm::Error::Wasm(format!("Wasm runtime error: {:?}", e.0))
	}
}

/// Wasm interpreter instance
pub struct WasmInterpreter {
	result: Vec<u8>,
}

impl WasmInterpreter {
	/// New wasm interpreter instance
	pub fn new() -> Result<WasmInterpreter, Error> {
		Ok(WasmInterpreter {
			result: Vec::with_capacity(DEFAULT_RESULT_BUFFER),
		})
	}
}

impl From<runtime::Error> for vm::Error {
	fn from(e: runtime::Error) -> Self {
		vm::Error::Wasm(format!("Wasm runtime error: {:?}", e))
	}
}

impl vm::Vm for WasmInterpreter {

	fn exec(&mut self, params: ActionParams, ext: &mut vm::Ext) -> vm::Result<GasLeft> {
		use parity_wasm::elements::Deserialize;

		let code = params.code.expect("exec is only called on contract with code; qed");

		trace!(target: "wasm", "Started wasm interpreter with code.len={:?}", code.len());

		let module: elements::Module = parity_wasm::deserialize_buffer(&*code)
			.map_err(|err| vm::Error::Wasm(format!("Error deserializing contract code ({:?})", err)))?;

		let loaded_module = wasmi::Module::from_parity_wasm_module(module)
			.map_err(|e| vm::Error::from(Error(e)))?;

		let instantiation_resolover = env::ImportResolver::with_limit(64);

		let module_instance = wasmi::ModuleInstance::new(
			&loaded_module,
			&wasmi::ImportsBuilder::new().with_resolver("env", &instantiation_resolover)
		).map_err(|e| vm::Error::from(Error(e)))?;

		if params.gas > ::std::u64::MAX.into() {
			return Err(vm::Error::Wasm("Wasm interpreter cannot run contracts with gas >= 2^64".to_owned()));
		}

		let initial_memory = instantiation_resolover.memory_size()
			.map_err(Error)?;
		trace!(target: "wasm", "Contract requested {:?} pages of initial memory", initial_memory);

		let mut runtime = Runtime::with_params(
			ext,
			instantiation_resolover.memory_ref().map_err(|e| vm::Error::from(Error(e)))?,
			params.gas.low_u64(),
			RuntimeContext {
				address: params.address,
				sender: params.sender,
				origin: params.origin,
				code_address: params.code_address,
				value: params.value.value(),
			},
		);

		// cannot overflow if static_region < 2^16,
		// initial_memory <- 0..2^32
		// total_charge <- static_region * 2^32 * 2^16
		// total_charge should be < 2^64
		// qed
		assert!(runtime.schedule().wasm.static_region < 2^16);
		runtime.charge(|s| initial_memory as u64 * 64 * 1024 * s.wasm.static_region as u64)?;

		// let (mut cursor, data_position) = match params.params_type {
		// 	vm::ParamsType::Embedded => {
		// 		let module_size = parity_wasm::peek_size(&*code);
		// 		(
		// 			::std::io::Cursor::new(&code[..module_size]),
		// 			module_size
		// 		)
		// 	},
		// 	vm::ParamsType::Separate => {
		// 		(::std::io::Cursor::new(&code[..]), 0)
		// 	},
		// };

		// let contract_module = wasm_utils::inject_gas_counter(
		// 	elements::Module::deserialize(
		// 		&mut cursor
		// 	).map_err(|err| {
		// 		vm::Error::Wasm(format!("Error deserializing contract code ({:?})", err))
		// 	})?,
		// 	runtime.gas_rules(),
		// );

		// let data_section_length = contract_module.data_section()
		// 	.map(|section| section.entries().iter().fold(0, |sum, entry| sum + entry.value().len()))
		// 	.unwrap_or(0)
		// 	as u64;

		// let static_segment_cost = data_section_length * runtime.ext().schedule().wasm.static_region as u64;
		// runtime.charge(|_| static_segment_cost).map_err(Error)?;

		// let d_ptr = {
		// 	match params.params_type {
		// 		vm::ParamsType::Embedded => {
		// 			runtime.write_descriptor(
		// 				if data_position < code.len() { &code[data_position..] } else { &[] }
		// 			).map_err(Error)?
		// 		},
		// 		vm::ParamsType::Separate => {
		// 			runtime.write_descriptor(&params.data.unwrap_or_default())
		// 				.map_err(Error)?
		// 		}
		// 	}
		// };

		// {
		// 	let execution_params = runtime.execution_params()
		// 		.add_argument(interpreter::RuntimeValue::I32(d_ptr.as_raw() as i32));

		// 	let module_instance = self.program.add_module("contract", contract_module, Some(&execution_params.externals))
		// 		.map_err(|err| {
		// 			trace!(target: "wasm", "Error adding contract module: {:?}", err);
		// 			vm::Error::from(Error(err))
		// 		})?;

		// 	match module_instance.execute_export("_call", execution_params) {
		// 		Ok(_) => { },
		// 		Err(interpreter::Error::User(UserTrap::Suicide)) => { },
		// 		Err(err) => {
		// 			trace!(target: "wasm", "Error executing contract: {:?}", err);
		// 			return Err(vm::Error::from(Error(err)))
		// 		}
		// 	}
		// }

		// let result = result::WasmResult::new(d_ptr);
		// if result.peek_empty(&*runtime.memory()).map_err(|e| Error(e))? {
		// 	trace!(target: "wasm", "Contract execution result is empty.");
		// 	Ok(GasLeft::Known(runtime.gas_left()?.into()))
		// } else {
		// 	self.result.clear();
		// 	// todo: use memory views to avoid copy
		// 	self.result.extend(result.pop(&*runtime.memory()).map_err(|e| Error(e.into()))?);
		// 	let len = self.result.len();
		// 	Ok(GasLeft::NeedsReturn {
		// 		gas_left: runtime.gas_left().map_err(|e| Error(e.into()))?.into(),
		// 		data: ReturnData::new(
		// 			::std::mem::replace(&mut self.result, Vec::with_capacity(DEFAULT_RESULT_BUFFER)),
		// 			0,
		// 			len,
		// 		),
		// 		apply_state: true,
		// 	})
		// }

		Ok(GasLeft::Known(0.into()))

	}
}
