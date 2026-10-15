-- AVNI Drive Scanner: the file index ("processed" data -> Supabase). Run once in the Supabase SQL editor.
-- Only metadata, hashes and verdicts are stored here; the files themselves go to Google Drive.
create table if not exists public.drive_files (
  machine      text        not null,
  drive        text        not null,
  path         text        not null,
  name         text        not null,
  ext          text,
  is_dir       boolean     not null default false,
  size         bigint      not null default 0,
  created_at   timestamptz,
  modified_at  timestamptz,
  accessed_at  timestamptz,
  attributes   integer,
  links        integer,
  blake3       text,
  sha256       text,
  sample_hash  text,
  dup_group    integer,
  verdict      text        not null,
  reason       text,
  scanned_at   timestamptz not null,
  primary key (machine, drive, path)
);
create index if not exists drive_files_blake3_idx  on public.drive_files (blake3);
create index if not exists drive_files_verdict_idx on public.drive_files (verdict);
create index if not exists drive_files_name_idx    on public.drive_files (lower(name));
-- Row level security on, no policies: only the service-role key used by the app can read or write.
alter table public.drive_files enable row level security;
