import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'
import { applyTheme, initialTheme } from './lib/theme'

// Apply the persisted theme BEFORE mounting so the webview never
// flashes the wrong palette (dark is the default).
applyTheme(initialTheme())

const app = mount(App, {
  target: document.getElementById('app')!,
})

export default app
