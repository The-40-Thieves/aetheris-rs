const { invoke } = window.__TAURI__.core;

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
        let totalMem = stats.memory.total_memory || 0;
        let usedMem = stats.memory.used_memory || 0;
        let memPercent = totalMem > 0 ? (usedMem / totalMem) * 100 : 0;
        
        document.getElementById('cpu-usage').innerText = `${stats.cpu.global_usage.toFixed(1)}%`;
        document.getElementById('cpu-bar').style.width = `${stats.cpu.global_usage}%`;
        document.getElementById('cpu-bar').className = `fill ${stats.cpu.global_usage > 85 ? 'danger' : stats.cpu.global_usage > 60 ? 'warning' : ''}`;

        document.getElementById('mem-usage').innerText = `${formatBytes(usedMem)} / ${formatBytes(totalMem)}`;
        document.getElementById('mem-bar').style.width = `${memPercent}%`;
        document.getElementById('mem-bar').className = `fill ${memPercent > 85 ? 'danger' : memPercent > 60 ? 'warning' : ''}`;

        document.getElementById('uptime').innerText = `${Math.floor(stats.uptime / 3600)}h ${Math.floor((stats.uptime % 3600)/60)}m`;

        // --- GPU ---
        let gpuHtml = '';
        if (stats.gpus && stats.gpus.length > 0) {
            stats.gpus.forEach(gpu => {
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

        // --- Storage & RUL ---
        let storageHtml = '';
        if (stats.smartDisks && stats.smartDisks.length > 0) {
            stats.smartDisks.forEach(disk => {
                let rul = disk.rul || {};
                let estDate = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : 'N/A';
                storageHtml += `
                    <div class="metric highlight">
                        <span class="label">${disk.device} - ${disk.model}</span>
                        <span class="value">SOH: ${rul.healthPercent ? rul.healthPercent.toFixed(2) : '--'}%</span>
                        <span class="label" style="color: var(--warning)">Est. EOL: ${estDate}</span>
                        <span class="label">Written: ${formatBytes(disk.bytesWritten)}</span>
                    </div>
                `;
            });
        } else {
            // Fallback to basic disks if SMART isn't available
            stats.disks.forEach(disk => {
                storageHtml += `
                    <div class="metric">
                        <span class="label">${disk.name} (${disk.fs_type})</span>
                        <span class="value">${formatBytes(disk.total_space - disk.available_space)} / ${formatBytes(disk.total_space)}</span>
                    </div>
                `;
            });
        }
        document.getElementById('storage-content').innerHTML = storageHtml || `<div class="empty-state">No Disks</div>`;

        // --- Battery & RUL ---
        let battHtml = '';
        if (stats.batteries && stats.batteries.length > 0) {
            stats.batteries.forEach(batt => {
                let rul = batt.rul || {};
                let estDate = rul.estimatedEndOfLife ? new Date(rul.estimatedEndOfLife).toLocaleDateString() : 'N/A';
                battHtml += `
                    <div class="metric highlight">
                        <span class="label">SOH: ${batt.stateOfHealth.toFixed(1)}% | Cycles: ${batt.cycleCount}</span>
                        <span class="value">${batt.stateOfCharge.toFixed(1)}% (${batt.state})</span>
                        <span class="label" style="color: var(--warning)">Est. End of Optimal Life: ${estDate}</span>
                    </div>
                `;
            });
        } else {
            battHtml = `<div class="empty-state">Desktop system (No battery)</div>`;
        }
        document.getElementById('battery-content').innerHTML = battHtml;

        // --- Egress Topology ---
        let egressHtml = '';
        if (stats.egressTopology && stats.egressTopology.length > 0) {
            stats.egressTopology.forEach(conn => {
                egressHtml += `
                    <tr>
                        <td style="${conn.shadow_alert ? 'color: var(--danger)' : ''}">${conn.process} (PID ${conn.pid})</td>
                        <td>${conn.destination_name} <br><span class="label" style="font-size: 0.75rem">${conn.destination_ip}</span></td>
                        <td>${formatBytes(conn.bytes_sent)}</td>
                        <td>${conn.provider}</td>
                        <td style="color: ${conn.estimated_cost_usd > 0 ? 'var(--warning)' : 'var(--success)'}">
                            $${conn.estimated_cost_usd.toFixed(2)}
                        </td>
                    </tr>
                `;
            });
        } else {
            egressHtml = `<tr><td colspan="5" class="empty-state">No active egress connections detected.</td></tr>`;
        }
        document.getElementById('egress-body').innerHTML = egressHtml;

        // --- External Baselines ---
        if (stats.externalBaselines) {
            // DORA
            let dbm = stats.externalBaselines.dora.benchmarks;
            let dlm = stats.externalBaselines.dora.local_metrics;
            document.getElementById('dora-body').innerHTML = `
                <tr><td>Deploy Freq</td><td>${dlm.deploy_frequency}</td><td>${dbm.deploy_frequency.elite}</td></tr>
                <tr><td>Lead Time</td><td>${dlm.lead_time}</td><td>${dbm.lead_time.elite}</td></tr>
                <tr><td>Change Failure</td><td>${dlm.change_failure_rate}</td><td>${dbm.change_failure_rate.elite}</td></tr>
                <tr><td>MTTR</td><td>${dlm.mttr}</td><td>${dbm.mttr.elite}</td></tr>
            `;

            // Pricing
            let prHtml = '';
            stats.externalBaselines.observability_pricing.forEach(p => {
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
            stats.externalBaselines.ai_evals.lmsys_chat_arena.forEach(ai => {
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
