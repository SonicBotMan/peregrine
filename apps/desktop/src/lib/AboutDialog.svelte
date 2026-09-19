<script lang="ts">
  /**
   * About dialog (GUI-verify batch-3): identity, bilingual pitch,
   * feature highlights and open-source acknowledgments. Content is
   * static except the version, which rides in from the shell.
   */
  import { X, Zap, Bot, Layers, MonitorSmartphone, Heart, ExternalLink } from '@lucide/svelte';

  let {
    onClose,
    version,
  }: {
    onClose: () => void;
    version: string;
  } = $props();

  const REPO = 'https://github.com/SonicBotMan/peregrine';

  const FEATURES = [
    {
      icon: Zap,
      title: 'IDM-class acceleration',
      desc: 'Rust multi-segment kernel · byte-exact resume · adaptive concurrency',
      cn: '自研 Rust 多段加速内核，kill -9 级断点续传',
    },
    {
      icon: Bot,
      title: 'AI-native MCP',
      desc: 'Let Claude / any agent drive your downloads over MCP',
      cn: 'AI Agent 原生 MCP 调度，10 tools · 3 resources · live events',
    },
    {
      icon: Layers,
      title: 'One daemon, three clients',
      desc: 'CLI, GUI and MCP are thin clients over the same REST + WS core',
      cn: 'CLI / GUI / MCP 三端同构的 headless 架构',
    },
    {
      icon: MonitorSmartphone,
      title: 'Desktop-grade GUI',
      desc: 'Tauri 2 + Svelte 5 — dense, keyboard-first, dual theme',
      cn: '工具级密度的桌面 GUI，OKLCH 双主题',
    },
  ];

  const THANKS = [
    'Tauri 2',
    'Svelte 5',
    'bits-ui',
    'Tailwind CSS',
    'Lucide',
    'Inter',
    'tokio',
    'hyper',
    'axum',
    'librqbit',
    'rmcp',
  ];

  const onKeydown = (e: KeyboardEvent) => {
    if (e.key === 'Escape') onClose();
  };
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="dialog about"
    onclick={(e) => e.stopPropagation()}
    role="dialog"
    aria-label="About Peregrine"
    tabindex={-1}
  >
    <button class="close" title="Close" aria-label="Close" onclick={onClose}>
      <X size={14} />
    </button>

    <div class="hero">
      <img src="./icons/128x128.png" alt="Peregrine" class="logo" />
      <div class="id">
        <h2>Peregrine <span class="ver">{version}</span></h2>
        <p class="cn">游隼 — 俯冲时速 389 km/h 的地球最快动物，同时捕猎多个目标。</p>
        <p class="en">The fastest animal on Earth, diving at 389 km/h — hunting multiple targets at once.</p>
      </div>
    </div>

    <p class="pitch">
      Linux 下载器：IDM 级多段加速内核 + AI Agent 原生 MCP 调度，CLI / GUI / MCP 三端同构。<br />
      <span class="en">A Linux downloader with an IDM-class multi-segment kernel and native AI-agent scheduling over MCP.</span>
    </p>

    <div class="feats">
      {#each FEATURES as f (f.title)}
        <div class="feat">
          <span class="ficon"><f.icon size={14} /></span>
          <div class="ftext">
            <span class="ftitle">{f.title}</span>
            <span class="fdesc">{f.desc} · {f.cn}</span>
          </div>
        </div>
      {/each}
    </div>

    <div class="thanks">
      <p class="tlabel">Standing on the shoulders of open source</p>
      <p class="tlist">
        {#each THANKS as name, i (name)}
          {name}{i < THANKS.length - 1 ? ' ·' : ''}
        {/each}
      </p>
      <p class="tlicense">Apache-2.0 — clean-room implementation, no code shared with any prior art.</p>
    </div>

    <div class="btns">
      <a class="ctl" href={REPO} target="_blank" rel="noreferrer">
        <ExternalLink size={13} />
        GitHub
      </a>
      <button class="ctl primary" onclick={onClose}>Close</button>
    </div>
  </div>
</div>

<style>
  .about {
    width: min(620px, 92vw);
    padding: 22px 24px 18px;
  }
  .hero {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .logo {
    width: 64px;
    height: 64px;
    border-radius: 14px;
    flex-shrink: 0;
  }
  .id h2 {
    margin: 0 0 4px;
    font-size: 18px;
    font-weight: 650;
    color: var(--text);
  }
  .ver {
    font: 500 11px var(--font-mono);
    color: var(--dim);
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 5px;
    padding: 1px 6px;
    vertical-align: 2px;
    margin-left: 6px;
  }
  .cn {
    margin: 0;
    font-size: 12.5px;
    color: var(--text);
  }
  .en {
    margin: 2px 0 0;
    font-size: 11.5px;
    color: var(--dim);
  }
  .pitch {
    margin: 14px 0;
    padding: 10px 12px;
    background: var(--elevated);
    border: 1px solid var(--line-subtle);
    border-radius: 8px;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text);
  }
  .pitch .en {
    color: var(--dim);
    font-size: 11.5px;
  }
  .feats {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px 16px;
    margin-bottom: 14px;
  }
  .feat {
    display: flex;
    gap: 8px;
    align-items: flex-start;
  }
  .ficon {
    color: var(--accent);
    display: inline-flex;
    margin-top: 2px;
    flex-shrink: 0;
  }
  .ftext {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .ftitle {
    font-size: 12px;
    font-weight: 600;
    color: var(--text);
  }
  .fdesc {
    font-size: 11px;
    color: var(--dim);
    line-height: 1.45;
  }
  .thanks {
    border-top: 1px solid var(--line-subtle);
    padding-top: 12px;
    margin-bottom: 14px;
  }
  .tlabel {
    margin: 0 0 4px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .tlist {
    margin: 0 0 6px;
    font-size: 12px;
    color: var(--text);
    line-height: 1.7;
  }
  .tlicense {
    margin: 0;
    font-size: 11px;
    color: var(--dim);
  }
  .btns {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .btns .ctl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    text-decoration: none;
  }
</style>
