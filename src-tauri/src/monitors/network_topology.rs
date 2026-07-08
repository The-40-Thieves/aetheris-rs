use serde_json::json;
use std::sync::Arc;
use crate::database::Database;

pub fn get_egress_topology(_db: &Arc<Database>) -> Vec<serde_json::Value> {
    // In production, this pulls from the eBPF maps populated by our `aya` probe on Linux
    // or the Win32 `GetExtendedTcpTable` results on Windows.
    // We mock the discovered topology for this implementation to satisfy the frontend UI.

    let mut connections = Vec::new();

    // Mock an AWS Egress
    connections.push(json!({
        "process": "node",
        "pid": 1234,
        "destination_ip": "54.239.28.85",
        "destination_name": "AWS EC2 (us-east-1)",
        "bytes_sent": 1024 * 1024 * 543, // 543 MB
        "provider": "AWS",
        "estimated_cost_usd": 0.09 * 0.543, // $0.09 per GB
        "is_mesh": false,
    }));

    // Mock a Tailscale mesh connection
    connections.push(json!({
        "process": "ssh",
        "pid": 8842,
        "destination_ip": "100.112.55.21",
        "destination_name": "Home Server",
        "bytes_sent": 1024 * 1024 * 12, // 12 MB
        "provider": "Tailscale",
        "estimated_cost_usd": 0.0, // Mesh traffic is free
        "is_mesh": true,
    }));
    
    // Mock Shadow Cloud
    connections.push(json!({
        "process": "docker (idle-db)",
        "pid": 992,
        "destination_ip": "None",
        "destination_name": "None",
        "bytes_sent": 0,
        "provider": "Local",
        "estimated_cost_usd": 0.0,
        "is_mesh": false,
        "shadow_alert": true,
    }));

    connections
}
