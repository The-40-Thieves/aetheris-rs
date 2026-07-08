# Aetheris Telemetry

Aetheris is a next-generation "God-Tier" telemetry client built with Rust and Tauri. It bridges standard consumer machine monitoring with industrial-grade predictive analytics, networking observability, and AI model evaluation.

## Features

- **Deep Hardware Telemetry**: Monitors GPU stats via `nvidia-smi`, `rocm-smi`, and Apple `powermetrics`. Analyzes live SMART data for NVMe/SSDs and tracks true state-of-health and cycle counts for batteries.
- **Predictive RUL Modeling**: Leverages a local SQLite database (`aetheris_telemetry.db`) to track hardware degradation over time. Calculates and projects the Estimated End of Life (Date) for SSDs (using TBW ratings) and Laptop Batteries.
- **AI Observability & Proxies**: Implements an embedded Axum reverse-proxy (Port `3030`) that intercepts local LLM inference engines (like Ollama and LM Studio) to track Tokens per Second and Latency dynamically without requiring you to change your local apps.
- **Hybrid Egress Costing**: Highlights shadow cloud traffic and attributes real-world cost metrics to your data egress (e.g., AWS vs local Tailscale mesh traffic).
- **External Benchmarks**: Automatically contextualizes your local stats against industry standards. Embeds true Tjmax limits for chips, DORA reliability tiers, observability tool pricing, and live LMSYS LLM ranking data directly alongside your own metrics.
- **Glassmorphic UI**: Powered by Vanilla HTML/CSS/JS with a highly responsive CSS grid, live polling, and smooth micro-animations.

## Requirements

To extract the most accurate data possible (such as `smartctl` metrics or `powermetrics` on macOS), **Aetheris should be run with Administrator or Root privileges**.

- [Node.js](https://nodejs.org)
- [Rust & Cargo](https://rustup.rs/)
- `smartctl` (Required for detailed SSD endurance metrics)
- `nvidia-smi` (For NVIDIA GPU tracking)

## Getting Started

1. **Clone and Install dependencies**
   ```bash
   git clone https://github.com/The-40-Thieves/aetheris-rs.git
   cd aetheris-rs
   npm install
   ```

2. **Run the Development Server**
   ```bash
   npm run dev
   ```
   *Note: On Linux/Mac, you may want to run `sudo -E npm run dev` to grant aetheris access to SMART metrics and `powermetrics`.*

3. **Build the Final Application**
   ```bash
   npm run build
   ```
