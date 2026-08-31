import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import './styles.css'
import { useAppStore } from './stores/appStore'

// Kick off IPC bootstrap before first render so pages start with data.
void useAppStore.getState().bootstrap()

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
