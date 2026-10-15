import { QueryClient } from '@tanstack/react-query';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false, // a failed engine call should be shown, not hidden behind retries
      refetchOnWindowFocus: false,
      staleTime: 5_000,
    },
  },
});
