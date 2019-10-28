// evaluate a given graph by simulate a scheduler with profile data

use oh_my_rust::*;
use std::convert::TryInto;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::cmp;
use crate::graph::Target;
use crate::proto::types::DataType;
use crate::proto::attr_value::{AttrValue, AttrValue_oneof_value};
use crate::proto::node_def::NodeDef;
use crate::proto::tensor::TensorProto;

pub trait Scheduler {
    fn evaluate(&mut self, target: &Target) -> u64;
}

pub struct TensorFlowLikeScheduler {
    n: usize,
    profile_dict: BTreeMap<String, u64>
}

impl TensorFlowLikeScheduler {
    pub fn new(n: usize, profile_dict: BTreeMap<String, u64>) -> Self {
        Self { n, profile_dict }
    }

    fn profile(&self, node: &NodeDef, _device_id: usize) -> Option<u64> {
        let origin_name = node.attr.get("_tge_origin")?.get_s();
        let time = self.profile_dict.get(&String::from_utf8(origin_name.to_vec()).unwrap()).copied();
        // technically we do not need to know whether it is replicated if we use a profiler since it will be reflected by the input size.
        time.map(|x| if node.name.as_bytes() == origin_name { // not replicated
            x
        } else { // replicated
            x / self.n as u64
        })
    }
}

// a silly type just to satisfy the BinaryHeap API
#[derive(Debug, Eq, PartialEq)]
struct Task {
    id: usize,
    pub eft: u64
}

impl Ord for Task {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        Ord::cmp(&self.eft, &other.eft).reverse()
    }
}

impl PartialOrd for Task {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Scheduler for TensorFlowLikeScheduler {
    fn evaluate(&mut self, target: &Target) -> u64 {
        task!("evaluate graph of {} nodes", target.pb.node.len());

        let nodes = &target.pb.node;
        let name_dict: BTreeMap<_, _> = nodes.iter().enumerate().map(|(i, x)| (x.name.clone(), i)).collect();
        let device_dict: BTreeMap<_, _> = target.devices.iter().enumerate().map(|(i, x)| (x.clone(), i)).collect();

        // initialize two aux lists
        let mut input_list = vec![]; // the i-th element is a list of nodes that i-th node is waiting for
        let mut output_list = vec![]; // the i-th element is a list of nodes that is waiting for i-th node to complete
        for (i, node) in nodes.iter().enumerate() {
            input_list.push(Vec::with_capacity(node.input.len()));
            output_list.push(Vec::with_capacity(4));

            for input in node.input.iter() {
                let input_name = if input.starts_with('^') {
                    &input[1..]
                } else {
                    match input.find(':') {
                        Some(i) => &input[..i],
                        None => input
                    }
                };
                let input_id = name_dict[input_name];
                input_list[i].push(input_id);
                output_list[input_id].push(i);
            }
        }

        let mut time = 0;
        let mut ongoing_tasks = BinaryHeap::new();
        let mut ready_list: VecDeque<_> = input_list.iter().enumerate().filter(|(_, x)| x.is_empty()).map(|(i, _)| i).collect(); // TODO: find the nodes that actually need to be runned (can lead to the terminating node), or assume the DAG is already pruned.
        let mut gpu_avaliable_time = vec![0; target.devices.len()];

        loop {
            // schedule ready nodes. Note the scheduled nodes may or may not start immediatly depending on the GPU queue. There may be other nodes become ready before some nodes schedualed earlier actually start.
            while let Some(id) = ready_list.pop_front() {
                let device = device_dict[&nodes[id].device];
                let node = &nodes[id];
                let eft = cmp::max(gpu_avaliable_time[device], time) + self.profile(node, device).unwrap_or(0);
                gpu_avaliable_time[device] = eft;
                ongoing_tasks.push(Task { id, eft });
            }

            // move a time step forward
            if let Some(Task { id, eft }) = ongoing_tasks.pop() {
                time = eft;
                for output in &output_list[id] {
                    let list = &mut input_list[*output];
                    list.retain(|x| *x != id);
                    if list.is_empty() {
                        ready_list.push_back(*output)
                    }
                }
            } else { // finally done
                break
            }
        }

        time
    }
}
