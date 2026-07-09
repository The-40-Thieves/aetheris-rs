const { invoke } = window.__TAURI__.core;

// Escape untrusted strings (process names, device models, etc.) before they are
// interpolated into innerHTML, to prevent HTML/script injection from crafted
// process names or device identifiers.
function esc(s) {
    return String(s ?? '')
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;')
        .replaceAll("'", '&#39;');
}

function formatBytes(bytes, decimals = 2) {
    if (!+bytes) return '0 Bytes';
    const k = 1024;
    const dm = decimals < 0 ? 0 : decimals;
    const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`;
}

function updateDOM() {
    invoke("get_stats").then((stats) => {
        // --- Overview ---
        let totalMem = stats.static.mem.total || 0;
        let usedMem = stats.dynamic.mem.used || 0;
        let memPercent = totalMem > 0 ? (usedMem / totalMem) * 100 : 0;
        
        document.getElementById('cpu-usage').innerText = `${stats.dynamic.cpu.currentLoad.toFixed(1)}%`;
        document.getElementById('cpu-bar').style.width = `${stats.dynamic.cpu.currentLoad}%`;
        document.getElementById('cpu-bar').className = `fill ${stats.dynamic.cpu.currentLoad > 85 ? 'danger' : stats.dynamic.cpu.currentLoad > 60 ? 'warning' : ''}`;

        document.getElementById('mem-usage').innerText = `${formatBytes(usedMem)} / ${formatBytes(totalMem)}`;
        document.getElementById('mem-bar').style.width = `${memPercent}%`;
        document.getElementById('mem-bar').className = `fill ${memPercent > 85 ? 'danger' : memPercent > 60 ? 'warning' : ''}`;

        let swapTotal = stats.dynamic.mem.swaptotal || 0;
        let swapUsed = stats.dynamic.mem.swapused || 0;
        let swapPercent = swapTotal > 0 ? (swapUsed / swapTotal) * 100 : 0;
        document.getElementById('swap-usage').innerText = `${formatBytes(swapUsed)} / ${formatBytes(swapTotal)}`;
        document.getElementById('swap-bar').style.width = `${swapPercent}%`;
        document.getElementById('swap-bar').className = `fill ${swapPercent > 85 ? 'danger' : swapPercent > 40 ? 'warning' : ''}`;
        
        let netRx = 0;
        let netTx = 0;
        if (stats.dynamic.network) {
            stats.dynamic.network.forEach(n => {
                netRx += n.rx_sec;
                netTx += n.tx_sec;
            });
        }
        document.getElementById('net-io').innerText = `↓ ${formatBytes(netRx)}/s | ↑ ${formatBytes(netTx)}/s`;

        document.getElementById('uptime').innerText = `${Math.floor(stats.dynamic.uptime / 3600)}h ${Math.floor((stats.dynamic.uptime % 3600)/60)}m`;

        // --- Processes ---
        let procHtml = '';
        if (stats.dynamic.processes && stats.dynamic.processes.listCpu) {
            stats.dynamic.processes.listCpu.forEach(p => {
                procHtml += `
                    <tr>
                        <td>${p.pid}</td>
                        <td>${p.name}</td>
                        <td style="color: ${p.cpu > 50 ? 'var(--danger)' : 'inherit'}">${p.cpu.toFixed(1)}%</td>
                        <td>${p.mem.toFixed(1)}%</td>
                        <td><span style="font-size:0.75rem">R: ${formatBytes(p.disk_r)}/s <br> W: ${formatBytes(p.disk_w)}/s</span></td>
                    </tr>
                `;
            });
        }
        document.getElementById('processes-body').innerHTML = procHtml || `<tr><td colspan="4">No process data</td></tr>`;

        // --- Sensors ---
        let sensorHtml = '';
        if (stats.dynamic.extras && stats.dynamic.extras.sensors) {
            stats.dynamic.extras.sensors.forEach(s => {
                let color = s.temp > (s.critical || 90) ? 'var(--danger)' : s.temp > (s.max || 80) ? 'var(--warning)' : 'inherit';
                sensorHtml += `
                    <tr>
                        <td>${s.label}</td>
                        <td style="color: ${color}">${s.temp.toFixed(1)}°C</td>
                        <td><span class="label" style="font-size: 0.75rem">Max: ${s.max || '--'}</span></td>
                    </tr>
                `;
            });
            if (stats.dynamic.extras.sensors.length === 0) {
                sensorHtml = `<tr><td class="empty-state">No sensors detected</td></tr>`;
            }
        }
        document.getElementById('sensors-body').innerHTML = sensorHtml;

        // --- GPU ---
        let gpuHtml = '';
        if (stats.dynamic.extras.gpus && stats.dynamic.extras.gpus.length > 0) {
            stats.dynamic.extras.gpus.forEach(gpu => {
                let vramText = gpu.vramTotal > 0 ? `${formatBytes(gpu.vramUsed * 1024 * 1024)} / ${formatBytes(gpu.vramTotal * 1024 * 1024)}` : 'Unified Memory';
                gpuHtml += `
                    <div class="metric">
                        <span class="label">${gpu.vendor} - ${gpu.model}</span>
                        <span class="value">${gpu.load.toFixed(1)}% | ${gpu.temp.toFixed(1)}°C</span>
                        <span class="label">VRAM: ${vramText}</span>
                    </div>
                `;
            });
        } else {
            gpuHtml = `<div class="empty-state">No GPUs detected via tools</div>`;
        }
        document.getElementById('gpu-content').innerHTML = gpuHtml;

        // --- AI Proxy Observability (real telemetry, not a static label) ---
        const ai = (stats.dynamic.extras && stats.dynamic.extras.aiProxy) || {};
        const tpsEl = document.getElementById('ai-tps');
        const aiStatusEl = document.getElementById('ai-proxy-status');
        if (tpsEl) {
            tpsEl.innerText = ai.lastTokensPerSec != null
                ? `${ai.lastTokensPerSec.toFixed(1)} tok/s (last request)`
                : 'No traffic yet';
        }
        if (aiStatusEl) {
            const n = ai.samples || 0;
            aiStatusEl.innerText = `Proxy on ${esc(ai.proxyAddr || '127.0.0.1:3030')} · ${n} request${n === 1 ? '' : 's'} observed`;
        }

        // --- Storage & RUL ---
        let storageHtml = '';
        if (stats.dynamic.extras.smartDisks && stats.dynamic.extras.smartDisks.length > 0) {
            stats.dynamic.extras.smartDisks.forEach(disk => {
                let rul = disk.rul || {};
                let estDate = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : 'N/A';
                // Mark projections that fall back to the default velocity (too
                // little history) so they aren't mistaken for a measured trend.
                let estMark = rul.confidence === 'low' ? ' (est.)' : '';
                storageHtml += `
                    <div class="metric highlight">
                        <span class="label">${esc(disk.device)} - ${esc(disk.model)}</span>
                        <span class="value">SOH: ${rul.healthPercent != null ? rul.healthPercent.toFixed(2) : '--'}%</span>
                        <span class="label" style="color: var(--warning)">Est. EOL: ${estDate}${estMark}</span>
                        <span class="label">Written: ${formatBytes(disk.bytesWritten)}</span>
                    </div>
                `;
            });
        } else if (stats.dynamic.disk) {
            // Fallback to basic disks if SMART isn't available
            stats.dynamic.disk.forEach(disk => {
                storageHtml += `
                    <div class="metric">
                        <span class="label">${disk.mount}</span>
                        <span class="value">${formatBytes(disk.used)} / ${formatBytes(disk.size)}</span>
                    </div>
                `;
            });
        }
        document.getElementById('storage-content').innerHTML = storageHtml || `<div class="empty-state">No Disks</div>`;

        // --- Battery & RUL ---
        let battHtml = '';
        if (stats.dynamic.extras.batteries && stats.dynamic.extras.batteries.length > 0) {
            stats.dynamic.extras.batteries.forEach(batt => {
                let rul = batt.rul || {};
                let estDate = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : 'N/A';
                let estMark = rul.confidence === 'low' ? ' (est.)' : '';
                battHtml += `
                    <div class="metric highlight">
                        <span class="label">SOH: ${batt.stateOfHealth.toFixed(1)}% | Cycles: ${batt.cycleCount}</span>
                        <span class="value">${batt.stateOfCharge.toFixed(1)}% (${esc(batt.state)})</span>
                        <span class="label" style="color: var(--warning)">Est. End of Optimal Life: ${estDate}${estMark}</span>
                    </div>
                `;
            });
        } else {
            battHtml = `<div class="empty-state">Desktop system (No battery)</div>`;
        }
        document.getElementById('battery-content').innerHTML = battHtml;

        // --- Egress Topology ---
        // Values may legitimately be null: bytes_sent is null when tcp_info is
        // unavailable, and estimated_cost_usd is null when bytes are unknown or
        // the provider is unattributed. Render those honestly, never as $0.00.
        let egressHtml = '';
        if (stats.dynamic.extras.egressTopology && stats.dynamic.extras.egressTopology.length > 0) {
            stats.dynamic.extras.egressTopology.forEach(conn => {
                const pidLabel = conn.pid != null ? ` (PID ${conn.pid})` : '';
                const sentCell = conn.bytes_sent != null
                    ? formatBytes(conn.bytes_sent)
                    : `<span class="label" title="Per-PID byte accounting requires the eBPF probe (roadmap)">—</span>`;
                let costCell;
                if (conn.estimated_cost_usd == null) {
                    costCell = `<span class="label">n/a</span>`;
                } else if (conn.is_mesh || conn.estimated_cost_usd === 0) {
                    costCell = `<span style="color: var(--success)">Free</span>`;
                } else {
                    costCell = `<span style="color: var(--warning)">$${conn.estimated_cost_usd.toFixed(2)}</span>`;
                }
                const conf = conn.attribution_confidence === 'fallback'
                    ? ` <span class="label" style="font-size:0.7rem" title="Coarse hardcoded ranges; live provider lists still loading">~</span>`
                    : '';
                egressHtml += `
                    <tr>
                        <td style="${conn.shadow_alert ? 'color: var(--danger)' : ''}">${esc(conn.process)}${pidLabel}</td>
                        <td>${esc(conn.destination_name)}${conf} <br><span class="label" style="font-size: 0.75rem">${esc(conn.destination_ip)}</span></td>
                        <td>${sentCell}</td>
                        <td>${esc(conn.provider)}</td>
                        <td>${costCell}</td>
                    </tr>
                `;
            });
        } else {
            egressHtml = `<tr><td colspan="5" class="empty-state">No active egress connections detected.</td></tr>`;
        }
        document.getElementById('egress-body').innerHTML = egressHtml;

        // --- External Baselines ---
        if (stats.dynamic.extras.externalBaselines) {
            // DORA
            let dbm = stats.dynamic.extras.externalBaselines.dora.benchmarks;
            let dlm = stats.dynamic.extras.externalBaselines.dora.local_metrics;
            document.getElementById('dora-body').innerHTML = `
                <tr><td>Deploy Freq</td><td>${dlm.deploy_frequency}</td><td>${dbm.deploy_frequency.elite}</td></tr>
                <tr><td>Lead Time</td><td>${dlm.lead_time}</td><td>${dbm.lead_time.elite}</td></tr>
                <tr><td>Change Failure</td><td>${dlm.change_failure_rate}</td><td>${dbm.change_failure_rate.elite}</td></tr>
                <tr><td>MTTR</td><td>${dlm.mttr}</td><td>${dbm.mttr.elite}</td></tr>
            `;

            // Pricing
            let prHtml = '';
            stats.dynamic.extras.externalBaselines.observability_pricing.forEach(p => {
                let color = p.est_monthly_cost === "$0" ? 'var(--success)' : 'var(--text-main)';
                prHtml += `<tr>
                    <td style="color: ${color}; font-weight: ${p.est_monthly_cost === "$0" ? 'bold' : 'normal'}">${p.tool}</td>
                    <td style="color: ${color}">${p.tier}</td>
                    <td style="color: ${color}">${p.est_monthly_cost}</td>
                </tr>`;
            });
            document.getElementById('pricing-body').innerHTML = prHtml;

            // AI Evals
            let aiHtml = '';
            stats.dynamic.extras.externalBaselines.ai_evals.lmsys_chat_arena.forEach(ai => {
                aiHtml += `<tr>
                    <td>#${ai.rank}</td>
                    <td>${ai.model}</td>
                    <td>${ai.score} ELO</td>
                </tr>`;
            });
            document.getElementById('ai-evals-body').innerHTML = aiHtml;
        }

    });
}

// Initial fetch and 1s polling
updateDOM();
setInterval(updateDOM, 1000);
