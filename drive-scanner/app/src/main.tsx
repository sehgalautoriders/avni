import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClientProvider } from '@tanstack/react-query';
import './index.css';
import { App } from './App';
import { queryClient } from './lib/queryClient';
import { systemTheme } from './lib/theme';
import { useApp } from './store';

// With no saved choice, follow the system light/dark setting live.
try {
  window
    .matchMedia('(prefers-color-scheme: dark)')
    .addEventListener('change', () => useApp.getState().followSystem(systemTheme()));
} catch {
  // very old webviews: the theme stays as chosen at start
}

const container = document.getElementById('root');
if (!container) throw new Error('The page has no #root element.');

createRoot(container).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
