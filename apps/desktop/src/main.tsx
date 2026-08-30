import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { hasTauriRuntime } from './api/desktopApi'
import './index.css'
import App from './App.tsx'

document.documentElement.classList.toggle('is-tauri', hasTauriRuntime())

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
