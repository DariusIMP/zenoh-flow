//
// Copyright (c) 2017, 2021 ADLINK Technology Inc.
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ADLINK zenoh team, <zenoh@adlink-labs.tech>
//

use std::collections::HashMap;

use crate::async_std::sync::{Arc, Mutex};
use crate::model::deadline::E2EDeadlineRecord;
use crate::model::link::PortDescriptor;
use crate::runtime::dataflow::instance::link::{LinkReceiver, LinkSender};
use crate::runtime::dataflow::instance::runners::operator::OperatorIO;
use crate::runtime::dataflow::instance::runners::{Runner, RunnerKind};
use crate::runtime::dataflow::node::SinkLoaded;
use crate::runtime::message::Message;
use crate::runtime::InstanceContext;
use crate::types::ZFResult;
use crate::{Context, NodeId, PortId, PortType, Sink, State, ZFError};
use async_trait::async_trait;
use libloading::Library;

// Do not reorder the fields in this struct.
// Rust drops fields in a struct in the same order they are declared.
// Ref: https://doc.rust-lang.org/reference/destructors.html
// We need the state to be dropped before the sink/lib, otherwise we
// will have a SIGSEV.
#[derive(Clone)]
pub struct SinkRunner {
    pub(crate) id: NodeId,
    pub(crate) context: InstanceContext,
    pub(crate) input: PortDescriptor,
    pub(crate) link: Arc<Mutex<Option<LinkReceiver<Message>>>>,
    pub(crate) state: Arc<Mutex<State>>,
    pub(crate) sink: Arc<dyn Sink>,
    pub(crate) library: Option<Arc<Library>>,
    pub(crate) end_to_end_deadlines: Vec<E2EDeadlineRecord>,
}

impl SinkRunner {
    pub fn try_new(context: InstanceContext, sink: SinkLoaded, io: OperatorIO) -> ZFResult<Self> {
        let (mut inputs, _) = io.take();
        let port_id = sink.input.port_id.clone();
        let link = inputs.remove(&port_id).ok_or_else(|| {
            ZFError::MissingOutput(format!(
                "Missing link for port < {} > for Sink: < {} >.",
                &port_id, &sink.id
            ))
        })?;

        Ok(Self {
            id: sink.id,
            context,
            input: sink.input,
            link: Arc::new(Mutex::new(Some(link))),
            state: sink.state,
            sink: sink.sink,
            library: sink.library,
            end_to_end_deadlines: sink.end_to_end_deadlines,
        })
    }
}

#[async_trait]
impl Runner for SinkRunner {
    fn get_id(&self) -> NodeId {
        self.id.clone()
    }
    fn get_kind(&self) -> RunnerKind {
        RunnerKind::Sink
    }
    async fn add_input(&self, input: LinkReceiver<Message>) -> ZFResult<()> {
        (*self.link.lock().await) = Some(input);
        Ok(())
    }

    async fn add_output(&self, _output: LinkSender<Message>) -> ZFResult<()> {
        Err(ZFError::SinkDoNotHaveOutputs)
    }

    async fn clean(&self) -> ZFResult<()> {
        let mut state = self.state.lock().await;
        self.sink.finalize(&mut state)
    }

    fn get_inputs(&self) -> HashMap<PortId, PortType> {
        let mut inputs = HashMap::with_capacity(1);
        inputs.insert(self.input.port_id.clone(), self.input.port_type.clone());
        inputs
    }

    fn get_outputs(&self) -> HashMap<PortId, PortType> {
        HashMap::with_capacity(0)
    }

    async fn get_outputs_links(&self) -> HashMap<PortId, Vec<LinkSender<Message>>> {
        HashMap::with_capacity(0)
    }

    async fn take_input_links(&self) -> HashMap<PortId, LinkReceiver<Message>> {
        let mut link_guard = self.link.lock().await;
        if let Some(link) = &*link_guard {
            let mut inputs = HashMap::with_capacity(1);
            inputs.insert(self.input.port_id.clone(), link.clone());
            *link_guard = None;
            return inputs;
        }
        HashMap::with_capacity(0)
    }

    async fn start_recording(&self) -> ZFResult<String> {
        Err(ZFError::Unsupported)
    }

    async fn stop_recording(&self) -> ZFResult<String> {
        Err(ZFError::Unsupported)
    }

    async fn is_recording(&self) -> bool {
        false
    }

    async fn run(&self) -> ZFResult<()> {
        let mut context = Context::default();

        loop {
            // Guards are taken at the beginning of each iteration to allow interleaving.
            if let Some(link) = &*self.link.lock().await {
                let mut state = self.state.lock().await;

                let (_, message) = link.recv().await?;
                let input = match message.as_ref() {
                    Message::Data(data_message) => {
                        if let Err(error) = self
                            .context
                            .runtime
                            .hlc
                            .update_with_timestamp(&data_message.timestamp)
                        {
                            log::error!(
                                "[Sink: {}][HLC] Could not update HLC with timestamp {:?}: {:?}",
                                self.id,
                                data_message.timestamp,
                                error
                            );
                        }
                        let now = self.context.runtime.hlc.new_timestamp();
                        let mut input = data_message.clone();

                        data_message
                            .end_to_end_deadlines
                            .iter()
                            .for_each(|e2e_deadline| {
                                if let Some(miss) = e2e_deadline.check(&self.id, &now) {
                                    input.missed_end_to_end_deadlines.push(miss);
                                }
                            });

                        input
                    }

                    Message::Control(_) => return Err(ZFError::Unimplemented),
                };

                self.sink.run(&mut context, &mut state, input).await?;
            }
        }
    }
}

#[cfg(test)]
#[path = "./tests/sink_e2e_deadline_tests.rs"]
mod e2e_deadline_tests;
