//
// Copyright (c) 2022 ZettaScale Technology
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ZettaScale Zenoh Team, <zenoh@zettascale.tech>
//

use crate::model::connector::{ZFConnectorKind, ZFConnectorRecord};
use crate::model::dataflow::descriptor::DataFlowDescriptor;
use crate::model::deadline::E2EDeadlineRecord;
use crate::model::link::{LinkDescriptor, PortDescriptor};
use crate::model::node::{OperatorRecord, SinkRecord, SourceRecord};
use crate::model::{InputDescriptor, OutputDescriptor};
use crate::serde::{Deserialize, Serialize};
use crate::types::{RuntimeId, ZFError, ZFResult};
use crate::{merge_configurations, NodeId, PortType};
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

///A `DataFlowRecord` is an instance of a [`DataFlowDescriptor`](`DataFlowDescriptor`).
///
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataFlowRecord {
    pub uuid: Uuid,
    pub flow: String,
    pub operators: HashMap<NodeId, OperatorRecord>,
    pub sinks: HashMap<NodeId, SinkRecord>,
    pub sources: HashMap<NodeId, SourceRecord>,
    pub connectors: Vec<ZFConnectorRecord>,
    pub links: Vec<LinkDescriptor>,
    pub end_to_end_deadlines: Option<Vec<E2EDeadlineRecord>>,
}

impl DataFlowRecord {
    /// Creates a new `DataFlowRecord` record from its YAML format.
    ///
    ///  # Errors
    /// A variant error is returned if deserialization fails.
    pub fn from_yaml(data: &str) -> ZFResult<Self> {
        serde_yaml::from_str::<DataFlowRecord>(data)
            .map_err(|e| ZFError::ParsingError(format!("{}", e)))
    }

    /// Creates a new `DataFlowRecord` from its JSON format.
    ///
    ///  # Errors
    /// A variant error is returned if deserialization fails.
    pub fn from_json(data: &str) -> ZFResult<Self> {
        serde_json::from_str::<DataFlowRecord>(data)
            .map_err(|e| ZFError::ParsingError(format!("{}", e)))
    }

    /// Returns the JSON representation of the `DataFlowRecord`.
    ///
    ///  # Errors
    /// A variant error is returned if serialization fails.
    pub fn to_json(&self) -> ZFResult<String> {
        serde_json::to_string(&self).map_err(|_| ZFError::SerializationError)
    }

    /// Returns the YAML representation of the `DataFlowRecord`.
    ///
    ///  # Errors
    /// A variant error is returned if serialization fails.
    pub fn to_yaml(&self) -> ZFResult<String> {
        serde_yaml::to_string(&self).map_err(|_| ZFError::SerializationError)
    }

    /// Returns the runtime mapping for the given node.
    pub fn find_node_runtime(&self, id: &str) -> Option<RuntimeId> {
        match self.operators.get(id) {
            Some(o) => Some(o.runtime.clone()),
            None => match self.sources.get(id) {
                Some(s) => Some(s.runtime.clone()),
                None => self.sinks.get(id).map(|s| s.runtime.clone()),
            },
        }
    }

    /// Returns the output type for the given node and port.
    pub fn find_node_output_type(&self, id: &str, output: &str) -> Option<PortType> {
        log::trace!("find_node_output_type({:?},{:?})", id, output);
        match self.operators.get(id) {
            Some(o) => o.get_output_type(output),
            None => match self.sources.get(id) {
                Some(s) => s.get_output_type(output),
                None => None,
            },
        }
    }

    /// Returns the input type for the given node and port.
    pub fn find_node_input_type(&self, id: &str, input: &str) -> Option<PortType> {
        log::trace!("find_node_input_type({:?},{:?})", id, input);
        match self.operators.get(id) {
            Some(o) => o.get_input_type(input),
            None => match self.sinks.get(id) {
                Some(s) => s.get_input_type(input),
                None => None,
            },
        }
    }

    /// Adds the links.
    ///
    /// If the nodes are mapped to different machines it adds the couple of
    /// connectors in between and creates the unique key expression
    /// for the data to flow in Zenoh.
    ///
    ///  # Errors
    /// A variant error is returned if validation fails.
    fn add_links(
        &mut self,
        links: &[LinkDescriptor],
        nodes_to_remove: &HashSet<NodeId>,
    ) -> ZFResult<()> {
        for l in links.iter().filter(|&l| {
            !nodes_to_remove.contains(&l.from.node) && !nodes_to_remove.contains(&l.to.node)
        }) {
            log::debug!("Adding link: {:?}…", l);
            let from_runtime = match self.find_node_runtime(&l.from.node) {
                Some(rt) => rt,
                None => {
                    log::error!("Could not find runtime for: {:?}", &l.from.node);
                    return Err(ZFError::Uncompleted(format!(
                        "Unable to find runtime for {}",
                        &l.from.node
                    )));
                }
            };

            let to_runtime = match self.find_node_runtime(&l.to.node) {
                Some(rt) => rt,
                None => {
                    log::error!("Could not find runtime for: {:?}", &l.to.node);
                    return Err(ZFError::Uncompleted(format!(
                        "Unable to find runtime for {}",
                        &l.to.node
                    )));
                }
            };

            let from_type = match self.find_node_output_type(&l.from.node, &l.from.output) {
                Some(t) => t,
                None => {
                    return Err(ZFError::PortNotFound((
                        l.from.node.clone(),
                        l.from.output.clone(),
                    )))
                }
            };

            let to_type = match self.find_node_input_type(&l.to.node, &l.to.input) {
                Some(t) => t,
                None => {
                    return Err(ZFError::PortNotFound((
                        l.to.node.clone(),
                        l.to.input.clone(),
                    )))
                }
            };

            if from_type != to_type {
                return Err(ZFError::PortTypeNotMatching((from_type, to_type)));
            }

            if from_runtime == to_runtime {
                log::debug!("Adding link: {:?}… OK", l);
                // link between nodes on the same runtime
                self.links.push(l.clone())
            } else {
                // link between node on different runtime
                // here we have to create the connectors information
                // and add the new links

                // creating zenoh resource name
                let z_resource_name = format!(
                    "/zf/data/{}/{}/{}/{}",
                    &self.flow, &self.uuid, &l.from.node, &l.from.output
                );

                // We only create a sender if none was created for the same resource. The rationale
                // is to avoid creating multiple publisher for the same resource in case an operator
                // acts as a multiplexor.
                if !self
                    .connectors
                    .iter()
                    .any(|c| c.kind == ZFConnectorKind::Sender && c.resource == z_resource_name)
                {
                    // creating sender
                    let sender_id = format!(
                        "sender-{}-{}-{}-{}",
                        &self.flow, &self.uuid, &l.from.node, &l.from.output
                    );
                    let sender = ZFConnectorRecord {
                        kind: ZFConnectorKind::Sender,
                        id: sender_id.clone().into(),
                        resource: z_resource_name.clone(),
                        link_id: PortDescriptor {
                            port_id: l.from.output.clone(),
                            port_type: from_type,
                        },

                        runtime: from_runtime,
                    };

                    // creating link between node and sender
                    let link_sender = LinkDescriptor {
                        from: l.from.clone(),
                        to: InputDescriptor {
                            node: sender_id.into(),
                            input: l.from.output.clone(),
                        },
                        size: None,
                        queueing_policy: None,
                        priority: None,
                    };

                    // storing info in the dataflow record
                    self.connectors.push(sender);
                    self.links.push(link_sender);
                }

                // creating receiver
                let receiver_id = format!(
                    "receiver-{}-{}-{}-{}",
                    &self.flow, &self.uuid, &l.to.node, &l.to.input
                );
                let receiver = ZFConnectorRecord {
                    kind: ZFConnectorKind::Receiver,
                    id: receiver_id.clone().into(),
                    resource: z_resource_name.clone(),
                    link_id: PortDescriptor {
                        port_id: l.to.input.clone(),
                        port_type: to_type,
                    },

                    runtime: to_runtime,
                };

                // Creating link between receiver and node
                let link_receiver = LinkDescriptor {
                    from: OutputDescriptor {
                        node: receiver_id.into(),
                        output: l.to.input.clone(),
                    },
                    to: l.to.clone(),
                    size: None,
                    queueing_policy: None,
                    priority: None,
                };

                // storing info in the data flow record
                self.connectors.push(receiver);
                self.links.push(link_receiver);
            }
        }

        Ok(())
    }
}

impl TryFrom<(DataFlowDescriptor, Uuid)> for DataFlowRecord {
    type Error = ZFError;

    fn try_from(d: (DataFlowDescriptor, Uuid)) -> Result<Self, Self::Error> {
        let (dataflow, id) = d;

        let DataFlowDescriptor {
            flow,
            operators,
            sources,
            sinks,
            mut links,
            mapping,
            deadlines,
            loops,
            global_configuration,
            flags,
        } = dataflow;

        let mapping = mapping.map_or(HashMap::new(), |m| m);

        let nodes_to_remove = if let Some(flags) = flags {
            super::flag::get_nodes_to_remove(&flags)
        } else {
            HashSet::new()
        };

        let deadlines = deadlines
            .map(|deadlines_desc| deadlines_desc.into_iter().map(|desc| desc.into()).collect());

        let mut dfr = DataFlowRecord {
            uuid: id,
            flow,
            operators: HashMap::with_capacity(operators.len()),
            sinks: HashMap::with_capacity(sinks.len()),
            sources: HashMap::with_capacity(sources.len()),
            connectors: Vec::new(),
            links: Vec::new(),
            end_to_end_deadlines: deadlines,
        };

        for o in operators
            .into_iter()
            .filter(|o| !nodes_to_remove.contains(&o.id))
        {
            let or = OperatorRecord {
                id: o.id.clone(),
                inputs: o.inputs,
                outputs: o.outputs,
                uri: o.uri,
                configuration: merge_configurations(global_configuration.clone(), o.configuration),
                runtime: mapping
                    .get(&o.id)
                    .ok_or(ZFError::MissingConfiguration)
                    .cloned()?,
                deadline: o.deadline.as_ref().map(|period| period.to_duration()),
                ciclo: None,
            };
            dfr.operators.insert(o.id, or);
        }

        for s in sources
            .into_iter()
            .filter(|s| !nodes_to_remove.contains(&s.id))
        {
            let sr = SourceRecord {
                id: s.id.clone(),
                period: s.period,
                output: s.output,
                uri: s.uri,
                configuration: merge_configurations(global_configuration.clone(), s.configuration),
                runtime: mapping
                    .get(&s.id)
                    .ok_or(ZFError::MissingConfiguration)
                    .cloned()?,
            };
            dfr.sources.insert(s.id, sr);
        }

        for s in sinks
            .into_iter()
            .filter(|s| !nodes_to_remove.contains(&s.id))
        {
            let sr = SinkRecord {
                id: s.id.clone(),
                input: s.input,
                uri: s.uri,
                configuration: merge_configurations(global_configuration.clone(), s.configuration),
                runtime: mapping
                    .get(&s.id)
                    .ok_or(ZFError::MissingConfiguration)
                    .cloned()?,
            };
            dfr.sinks.insert(s.id, sr);
        }

        // A Loop between two nodes is, in fact, a backward link.
        //
        // To add a link we first need to add an input to the Ingress and an output to the Egress
        // (as these ports were not declared by the user). We also must perform these additions to
        // the `validator` as it will check that everything is alright on the link we are about to
        // create.
        //
        // We finally add the loop information to the Ingress and Egress.
        if let Some(loops) = loops {
            // Ciclo is the italian word for "loop" — we cannot use "loop" as it’s a reserved
            // keyword.
            for ciclo in loops {
                // Update the ingress / egress.
                let ingress = dfr
                    .operators
                    .get_mut(&ciclo.ingress)
                    .ok_or(ZFError::GenericError)?;
                ingress.inputs.push(PortDescriptor {
                    port_id: ciclo.feedback_port.clone(),
                    port_type: ciclo.port_type.clone(),
                });
                ingress.ciclo = Some(ciclo.clone());

                let egress = dfr
                    .operators
                    .get_mut(&ciclo.egress)
                    .ok_or(ZFError::GenericError)?;
                egress.outputs.push(PortDescriptor {
                    port_id: ciclo.feedback_port.clone(),
                    port_type: ciclo.port_type.clone(),
                });
                egress.ciclo = Some(ciclo.clone());

                links.push(LinkDescriptor {
                    from: OutputDescriptor {
                        node: ciclo.egress,
                        output: ciclo.feedback_port.clone(),
                    },
                    to: InputDescriptor {
                        node: ciclo.ingress,
                        input: ciclo.feedback_port,
                    },
                    size: None,
                    queueing_policy: None,
                    priority: None,
                });
            }
        }

        dfr.add_links(&links, &nodes_to_remove)?;

        Ok(dfr)
    }
}

impl Hash for DataFlowRecord {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
        self.flow.hash(state);
    }
}

impl PartialEq for DataFlowRecord {
    fn eq(&self, other: &DataFlowRecord) -> bool {
        self.uuid == other.uuid && self.flow == other.flow
    }
}

impl Eq for DataFlowRecord {}
