// History arrays (max 30 points, representing 60 seconds at 2s intervals)
const MAX_HISTORY_POINTS = 30;
const cpuHistory = Array(MAX_HISTORY_POINTS).fill(0);
const memHistory = Array(MAX_HISTORY_POINTS).fill(0);
const outpostCpuHistory = Array(MAX_HISTORY_POINTS).fill(0);

let activeProcTab = 'cpu';

// Keep track of static information status
let isStaticLoaded = false;

function formatUptime(seconds) {
  const d = Math.floor(seconds / (3600*24));
  const h = Math.floor((seconds % (3600*24)) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  return `${h}h ${m}m`;
}

// Format bytes into readable string (e.g. GB, MB)
function formatBytes(bytes, decimals = 2) {
  if (bytes === 0) return '0 Bytes';
  const k = 1024;
  const dm = decimals < 0 ? 0 : decimals;
  const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return parseFloat((bytes / Math.pow(k, i)).toFixed(dm)) + ' ' + sizes[i];
}

// Convert network speed to readable format
function formatSpeed(bytesPerSec) {
  return formatBytes(bytesPerSec, 1) + '/s';
}

// Draw custom SVG charts
function drawTrendChart(svgId, historyData, colorClass) {
  const svg = document.getElementById(svgId);
  if (!svg) return;

  const width = 500;
  const height = 100;
  const pointsCount = historyData.length;
  
  if (pointsCount === 0) return;

  // Build path points
  const points = [];
  for (let i = 0; i < pointsCount; i++) {
    const x = (i / (pointsCount - 1)) * width;
    // Map value (0-100) to height (100-0)
    const val = Math.min(Math.max(historyData[i], 0), 100);
    const y = height - (val / 100) * height;
    points.push({ x, y });
  }

  // Generate line path
  let lineD = `M ${points[0].x} ${points[0].y}`;
  for (let i = 1; i < points.length; i++) {
    lineD += ` L ${points[i].x} ${points[i].y}`;
  }

  // Generate area path (closing the shape at the bottom)
  const areaD = `${lineD} L ${points[points.length - 1].x} ${height} L ${points[0].x} ${height} Z`;

  // Update SVG paths
  const linePath = svg.querySelector('.chart-line');
  const areaPath = svg.querySelector('.chart-area');

  if (linePath) linePath.setAttribute('d', lineD);
  if (areaPath) areaPath.setAttribute('d', areaD);
}

// Update radial progress circle
function updateRadialProgress(elementId, percent) {
  const circle = document.getElementById(elementId);
  if (!circle) return;

  const radius = circle.r.baseVal.value;
  const circumference = 2 * Math.PI * radius; // ~251.2 for r=40
  
  const offset = circumference - (percent / 100) * circumference;
  circle.style.strokeDasharray = circumference;
  circle.style.strokeDashoffset = offset;
}

// Main function to fetch metrics and update the UI
const { invoke } = window.__TAURI__.core;

async function fetchStats() {
  try {
    const data = await invoke('get_stats');
    
    // Update system metadata once
    if (!isStaticLoaded && data.static) {
      document.getElementById('host-name').textContent = data.static.os.hostname || 'Unknown';
      document.getElementById('os-info').textContent = `${data.static.os.distro} (${data.static.os.arch})`;
      document.getElementById('kernel-info').textContent = data.static.os.kernel || 'Unknown';
      document.getElementById('cpu-brand').textContent = data.static.cpu.brand || 'Unknown CPU';
      document.getElementById('cpu-cores-val').textContent = `${data.static.cpu.physicalCores} Cores / ${data.static.cpu.cores} Threads`;
      document.getElementById('mem-total-val').textContent = formatBytes(data.static.mem.total, 1);
      
      // Hardware Card Data
      document.getElementById('sys-mfg').textContent = data.static.system.manufacturer || 'Unknown';
      document.getElementById('sys-model').textContent = data.static.system.model || 'Unknown';
      document.getElementById('sys-virtual').textContent = data.static.system.virtual ? 'Yes' : 'No';
      document.getElementById('bios-vendor').textContent = data.static.bios.vendor || 'Unknown';
      document.getElementById('bios-vendor').title = data.static.bios.vendor || 'Unknown';
      document.getElementById('bios-version').textContent = data.static.bios.version || 'Unknown';
      document.getElementById('board-model').textContent = data.static.baseboard.model || 'Unknown';
      document.getElementById('cpu-speed').textContent = `${data.static.cpu.speed || '?'} GHz`;
      document.getElementById('cpu-virt').textContent = data.static.cpu.virtualization ? 'Enabled' : 'Disabled';
      document.getElementById('cpu-flags').textContent = data.static.cpu.flags || 'None';
      document.getElementById('cpu-flags').title = data.static.cpu.flags || 'None';

      isStaticLoaded = true;
    }
    
    // Dynamic uptime
    document.getElementById('uptime-val').textContent = formatUptime(data.dynamic.uptime);

    // --- CPU Metrics ---
    const cpuLoad = data.dynamic.cpu.currentLoad;
    document.getElementById('cpu-val-percent').textContent = `${cpuLoad}%`;
    document.getElementById('cpu-user-val').textContent = `${data.dynamic.cpu.currentLoadUser}%`;
    document.getElementById('cpu-system-val').textContent = `${data.dynamic.cpu.currentLoadSystem}%`;
    updateRadialProgress('cpu-radial', cpuLoad);

    // Update CPU history & chart
    cpuHistory.push(cpuLoad);
    cpuHistory.shift();
    drawTrendChart('cpu-trend', cpuHistory);

    // Core-by-core utilization UI
    const coresGrid = document.getElementById('cores-grid-data');
    if (coresGrid) {
      coresGrid.innerHTML = data.dynamic.cpu.currentLoadCpu.map((load, index) => `
        <div class="core-item">
          <div class="core-label-row">
            <span class="core-name">Core ${index}</span>
            <span class="core-val">${load}%</span>
          </div>
          <div class="progress-bar-container">
            <div class="progress-fill" style="width: ${load}%"></div>
          </div>
        </div>
      `).join('');
    }

    // --- Memory Metrics ---
    const memPercent = data.dynamic.mem.percentUsed;
    document.getElementById('mem-val-percent').textContent = `${memPercent}%`;
    document.getElementById('mem-used-val').textContent = formatBytes(data.dynamic.mem.active, 1);
    document.getElementById('mem-free-val').textContent = formatBytes(data.dynamic.mem.available, 1);
    document.getElementById('mem-swap-val').textContent = `${formatBytes(data.dynamic.mem.swapused, 1)} / ${formatBytes(data.dynamic.mem.swaptotal, 1)} (${data.dynamic.mem.swapPercentUsed}%)`;
    updateRadialProgress('mem-radial', memPercent);
    
    renderToolchains(data.dynamic.extras.toolchains || []);
    renderVPNs(data.dynamic.extras.vpns || []);
    renderAIs(data.dynamic.extras.ais || []);
    renderApps(data.dynamic.extras.apps || []);

    // Update Mem history & chart
    memHistory.push(memPercent);
    memHistory.shift();
    drawTrendChart('mem-trend', memHistory);

    // --- Disks Storage ---
    const disksContainer = document.getElementById('disks-container');
    if (disksContainer) {
      if (data.dynamic.disk && data.dynamic.disk.length > 0) {
        disksContainer.innerHTML = data.dynamic.disk.map(disk => {
          const isHighUsage = disk.usePercent > 80;
          return `
            <div class="disk-item">
              <div class="disk-info-header">
                <span class="disk-mount">${disk.mount}</span>
                <span class="disk-ratio">${formatBytes(disk.used, 1)} / ${formatBytes(disk.size, 1)} (${disk.usePercent}%)</span>
              </div>
              <div class="disk-progress-container">
                <div class="disk-progress-bar ${isHighUsage ? 'orange' : 'cyan'}" style="width: ${disk.usePercent}%"></div>
              </div>
            </div>
          `;
        }).join('');
      } else {
        disksContainer.innerHTML = '<div class="loading-placeholder">No disk devices detected.</div>';
      }
    }

    // --- Network Throughput ---
    const netContainer = document.getElementById('network-container');
    if (netContainer) {
      const activeInterfaces = data.dynamic.network.filter(n => n.operstate === 'up' || n.rx_sec > 0 || n.tx_sec > 0);
      
      if (activeInterfaces.length > 0) {
        netContainer.innerHTML = activeInterfaces.map(net => `
          <div class="net-interface-card" style="flex-direction: column; align-items: stretch; gap: 12px;">
            <div style="display: flex; justify-content: space-between; align-items: center;">
              <div class="net-meta">
                <span class="net-name">${net.iface}</span>
                <span class="net-state ${net.operstate === 'up' ? 'online' : 'offline'}">${net.operstate.toUpperCase()}</span>
              </div>
              <div class="net-speeds">
                <div class="net-speed-item">
                  <span class="net-speed-icon">▼</span>
                  <div class="net-speed-info">
                    <span class="label">DOWN</span>
                    <span class="value">${formatSpeed(net.rx_sec)}</span>
                  </div>
                </div>
                <div class="net-speed-item">
                  <span class="net-speed-icon">▲</span>
                  <div class="net-speed-info">
                    <span class="label">UP</span>
                    <span class="value">${formatSpeed(net.tx_sec)}</span>
                  </div>
                </div>
              </div>
            </div>
            <div style="display: flex; justify-content: space-between; font-size: 10px; font-family: var(--font-mono); color: var(--text-secondary); border-top: 1px dashed rgba(255,255,255,0.05); padding-top: 8px;">
              <span style="max-width: 60%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${net.ip4 || 'No IPv4'}${net.ip6 ? ' / ' + net.ip6 : ''}</span>
              <span>${net.mac || 'No MAC'}</span>
            </div>
          </div>
        `).join('');
      } else {
        netContainer.innerHTML = '<div class="loading-placeholder">No active network interfaces detected.</div>';
      }
    }

    // --- Processes Explorer ---
    document.getElementById('total-procs-val').textContent = data.dynamic.processes.all;
    document.getElementById('running-procs-val').textContent = data.dynamic.processes.running;

    const tbody = document.getElementById('processes-tbody');
    if (tbody) {
      const activeList = activeProcTab === 'cpu' ? data.dynamic.processes.listCpu : data.dynamic.processes.listMem;
      if (activeList && activeList.length > 0) {
        tbody.innerHTML = activeList.map(proc => `
          <tr>
            <td class="proc-pid">${proc.pid}</td>
            <td class="proc-name" title="${proc.name}">${proc.name}</td>
            <td class="proc-cpu">${proc.cpu}%</td>
            <td class="proc-mem">${proc.mem}%</td>
            <td><span class="proc-state ${proc.state}">${proc.state}</span></td>
            <td class="proc-user">${proc.user}</td>
          </tr>
        `).join('');
      } else {
        tbody.innerHTML = '<tr><td colspan="6" class="center-text">No processes found.</td></tr>';
      }
    }

    // --- Docker Containers ---
    const dockerTbody = document.getElementById('docker-tbody');
    if (dockerTbody) {
      const containers = data.dynamic.extras.containers;
      if (containers && containers.length > 0) {
        dockerTbody.innerHTML = containers.map(c => {
          const isHealthy = c.Status?.includes("(healthy)") || c.Status?.includes("Up");
          const statusClass = isHealthy ? 'healthy' : 'unhealthy';
          const serverClass = c.server === 'cave' ? 'cave' : 'outpost';
          
          return `
            <tr>
              <td><span class="server-badge ${serverClass}">${c.server}</span></td>
              <td class="mono" style="color: var(--accent-cyan)">${c.ID.slice(0, 12)}</td>
              <td style="font-weight: 500">${c.Names}</td>
              <td class="mono" style="font-size: 0.75rem; max-width: 250px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;">${c.Image}</td>
              <td><span class="status-pill ${statusClass}">${c.Status}</span></td>
            </tr>
          `;
        }).join('');
      } else {
        dockerTbody.innerHTML = '<tr><td colspan="5" class="center-text">No active container services detected.</td></tr>';
      }
    }

    // --- Outpost Metrics (Hetzner VPS) ---
    const outpost = data.dynamic.extras.outpostStats;
    const outpostStatus = document.getElementById('outpost-status');
    if (outpost && outpostStatus) {
      outpostStatus.textContent = outpost.status.toUpperCase();
      if (outpost.status === 'online') {
        outpostStatus.className = 'status-indicator active';
      } else {
        outpostStatus.className = 'status-indicator offline';
      }

      const outpostCpuFill = document.getElementById('outpost-cpu-bar');
      if (outpostCpuFill) outpostCpuFill.style.width = `${outpost.cpu}%`;
      document.getElementById('outpost-cpu-val').textContent = `${outpost.cpu}%`;
      document.getElementById('outpost-loadavg').textContent = `load: ${outpost.loadavg}`;

      const outpostMemFill = document.getElementById('outpost-mem-bar');
      if (outpostMemFill) outpostMemFill.style.width = `${outpost.memPercent}%`;
      document.getElementById('outpost-mem-val').textContent = `${outpost.memPercent}% (${outpost.memUsed.toFixed(1)} GB / ${outpost.memTotal.toFixed(1)} GB)`;

      const outpostDiskFill = document.getElementById('outpost-disk-bar');
      if (outpostDiskFill) outpostDiskFill.style.width = `${outpost.diskPercent}%`;
      document.getElementById('outpost-disk-val').textContent = `${outpost.diskPercent}% (${outpost.diskUsed.toFixed(1)} GB / ${outpost.diskTotal.toFixed(1)} GB)`;

      if (outpost.status === 'online') {
        outpostCpuHistory.push(outpost.cpu);
      } else {
        outpostCpuHistory.push(0);
      }
      outpostCpuHistory.shift();
      drawTrendChart('outpost-cpu-trend', outpostCpuHistory);
    }

    // Ensure status light matches connection
    const statusText = document.getElementById('system-status');
    statusText.textContent = 'ONLINE';
    statusText.className = 'status-indicator active';

  } catch (error) {
    console.error('Error fetching real-time diagnostics:', error);
    // Mark UI status as offline
    const statusText = document.getElementById('system-status');
    statusText.textContent = 'OFFLINE';
    statusText.className = 'status-indicator offline';
  }
}

function renderSensors(sensors) {
  const container = document.getElementById('sensors-container');
  if (!sensors || sensors.length === 0) {
    container.innerHTML = '<div class="text-muted">No sensors detected</div>';
    return;
  }
  let html = '';
  sensors.forEach(s => {
    html += `
      <div class="metric-row">
        <span style="max-width: 100px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;" title="${s.label}">${s.label}</span>
        <span class="value-highlight">${Math.round(s.temp)}°C</span>
      </div>
    `;
  });
  container.innerHTML = html;
}

function renderToolchains(toolchains) {
  const container = document.getElementById('languages-grid-container');
  if (!toolchains || toolchains.length === 0) {
    container.innerHTML = '<div class="text-muted">No toolchains detected</div>';
    return;
  }
  let html = '';
  toolchains.forEach(t => {
    html += `
      <div class="lang-item">
        <span class="lang-name">${t.name}</span>
        <span class="lang-version" title="${t.path}">DETECTED</span>
      </div>
    `;
  });
  container.innerHTML = html;
}

function renderVPNs(vpns) {
  const container = document.getElementById('vpn-mesh-container');
  if (!vpns || vpns.length === 0) {
    container.innerHTML = '<div class="text-muted">No VPN/Mesh detected</div>';
    return;
  }
  let html = '';
  vpns.forEach(v => {
    let status = v.running ? 'online' : (v.installed ? 'offline' : 'unknown');
    html += `
      <div class="node-item">
        <span class="status-dot ${status}"></span>
        <div class="node-info">
          <span class="node-name">${v.name}</span>
          <span class="node-ip">${v.running ? 'Running' : 'Installed'}</span>
        </div>
      </div>
    `;
  });
  container.innerHTML = html;
}

function renderAIs(ais) {
  const container = document.getElementById('ai-systems-container');
  if (!ais || ais.length === 0) {
    container.innerHTML = '<div class="text-muted">No AI systems detected</div>';
    return;
  }
  let html = '';
  ais.forEach(a => {
    let status = a.running ? 'online' : (a.installed ? 'offline' : 'unknown');
    html += `
      <div class="node-item">
        <span class="status-dot ${status}"></span>
        <div class="node-info">
          <span class="node-name">${a.name}</span>
          <span class="node-ip">${a.type}</span>
        </div>
      </div>
    `;
  });
  container.innerHTML = html;
}

function renderApps(apps) {
  const container = document.getElementById('apps-container');
  if (!apps || apps.length === 0) {
    container.innerHTML = '<div class="text-muted">No applications detected or supported on this OS.</div>';
    return;
  }
  
  let html = '';
  apps.forEach(app => {
    html += `
      <div class="metric-row" style="background: rgba(255,255,255,0.02); padding: 8px 12px; border-radius: 4px; border: 1px solid rgba(255,255,255,0.05);">
        <span style="overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; color: var(--text-primary); font-size: 0.9rem;">${app}</span>
      </div>
    `;
  });
  container.innerHTML = html;
}

// Process Tab logic
document.getElementById('tab-cpu')?.addEventListener('click', (e) => {
  e.stopPropagation();
  activeProcTab = 'cpu';
  document.getElementById('tab-cpu').classList.add('active');
  document.getElementById('tab-mem').classList.remove('active');
  fetchStats();
});
document.getElementById('tab-mem')?.addEventListener('click', (e) => {
  e.stopPropagation();
  activeProcTab = 'mem';
  document.getElementById('tab-mem').classList.add('active');
  document.getElementById('tab-cpu').classList.remove('active');
  fetchStats();
});

// Add a micro-refresh trigger on card clicks for user satisfaction
document.querySelectorAll('.monitor-card').forEach(card => {
  card.addEventListener('click', () => {
    fetchStats();
  });
});

// Run immediate update, then poll every 2 seconds
fetchStats();
setInterval(fetchStats, 2000);
