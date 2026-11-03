{ pkgs }:

# Reconciles a cang guest's /nix/store against its own Nix DB. Guest-init calls
# this from nix prep (`internal nix prep`), so the store is consistent before
# the daemon starts and before any nix wrapper hands out a ready marker.
#
# The fault it heals is the cang host-overlay /nix shape, not a corrupt store:
# `upper` survives image generations while `lowerdir` is one image, so the
# copied-up upper/var/nix/db can name a path only an older image supplied.
# Nix trusts a valid row and never re-substitutes it, so exec fails with ENOENT
# on a path that exists. Restoring the object (step 1) is preferred over
# deleting the row: the row already records the right narHash, the copy keeps
# every upper-only object and every Refs edge untouched, and neither the
# workspace cache nor the image closure is refetched.
#
#   cang-nix-store-repair [--dry-run] [--scratch DIR] [--store URI] [--keep-scratch]
#
# Never edits /nix/store by deleting objects, never runs a GC, and never
# touches a path the DB does not already claim.
pkgs.writeShellScriptBin "cang-nix-store-repair" ''
  set -uo pipefail

  TOOL=cang-nix-store-repair
  NIX=${pkgs.nix}/bin/nix
  NIX_STORE=${pkgs.nix}/bin/nix-store
  SQLITE=${pkgs.sqlite}/bin/sqlite3
  MKTEMP=${pkgs.coreutils}/bin/mktemp
  FIND=${pkgs.findutils}/bin/find
  GREP=${pkgs.gnugrep}/bin/grep
  SED=${pkgs.gnused}/bin/sed
  SORT=${pkgs.coreutils}/bin/sort
  COMM=${pkgs.coreutils}/bin/comm
  CP=${pkgs.coreutils}/bin/cp
  RM=${pkgs.coreutils}/bin/rm
  WC=${pkgs.coreutils}/bin/wc
  MKDIR=${pkgs.coreutils}/bin/mkdir
  HEAD=${pkgs.coreutils}/bin/head
  MV=${pkgs.coreutils}/bin/mv
  TR=${pkgs.coreutils}/bin/tr

  usage() {
    ${pkgs.bash}/bin/cat >&2 <<EOF_USAGE
  $TOOL: reconcile a cang guest /nix/store against its Nix DB.

  A cang workspace /nix is an overlay: the upper survives image generations, the
  lowerdir is the image of the moment. After an image move a path the old image
  supplied can be listed as valid in the (copied-up) DB while its contents are
  gone, and Nix never re-substitutes a valid path, so executing it fails with
  ENOENT on a path that exists.

  For each such path this tool, in order:
    1. fetches it into a scratch store and copies the object into /nix/store.
       The DB row already carries the right narHash, so Nix then verifies it;
       nothing else is fetched and no DB row is touched.
    2. only if no substituter has it, deregisters the row (Refs and
       DerivationOutputs edges included) so Nix substitutes or rebuilds it on
       next use instead of trusting a lie.

  Options:
    --dry-run          report the drift, repair nothing
    --scratch DIR      scratch store root (default /var/tmp/cang-nix-store-repair)
    --store URI        Nix store to repair (default: local, else auto)
    --import-db PATH   merge another generation's Nix DB (a path the cang host
                       parked at /nix/var/nix/.cang-previous-db/db.sqlite when it
                       seeded this overlay) into the live DB before reconciling
    --purge-only       never fetch: only deregister rows whose store object is
                       absent in the merged view (the right policy right after a
                       seeded generation, where absence means "went with the old
                       image", not "went missing")
    --keep-scratch     keep the scratch store for inspection
    -h, --help         this text

  Env:
    CANG_NIX_REPAIR_SUBSTITUTERS  space-separated substituter URIs
                                  (default: nix config substituters)
    CANG_NIX_REPAIR_NO_CHECK_SIGS unset signatures for every fetch

  Exit status: 0 nothing left to repair, 1 paths remain absent, 64 usage,
  69 the store or a tool is unavailable.
  EOF_USAGE
  }

  scratch=''${CANG_NIX_REPAIR_SCRATCH:-/var/tmp/cang-nix-store-repair}
  store=
  import_db=
  dry_run=0
  purge_only=0
  keep_scratch=0

  while [ $# -gt 0 ]; do
    case "$1" in
      --dry-run) dry_run=1; shift ;;
      --keep-scratch) keep_scratch=1; shift ;;
      --scratch)
        [ $# -ge 2 ] || { echo "$TOOL: --scratch needs a directory" >&2; exit 64; }
        scratch=$2; shift 2 ;;
      --import-db)
        [ $# -ge 2 ] || { echo "$TOOL: --import-db needs a database path" >&2; exit 64; }
        import_db=$2; shift 2 ;;
      --purge-only) purge_only=1; shift ;;
      --store)
        [ $# -ge 2 ] || { echo "$TOOL: --store needs a URI" >&2; exit 64; }
        store=$2; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      *) echo "$TOOL: unknown argument '$1'" >&2; usage; exit 64 ;;
    esac
  done

  for tool in $NIX $NIX_STORE $SQLITE; do
    if [ ! -x "$tool" ]; then
      echo "$TOOL: required tool '$tool' is missing" >&2
      exit 69
    fi
  done

  # Prefer the daemon when one is live (guest-init calls this before starting it,
  # so 'local' is the prep-time path); never touch the store directly while a
  # daemon owns the DB unless that is the only route.
  if [ -z "$store" ]; then
    if $NIX path-info --store auto --all >/dev/null 2>&1; then
      store=auto
    elif $NIX path-info --store local --all >/dev/null 2>&1; then
      store=local
    else
      echo "$TOOL: cannot read /nix/store with 'auto' or 'local'; run as root or with a live nix-daemon" >&2
      exit 69
    fi
  fi

  logical=/nix/store
  store_dir=/nix/store
  db_path=/nix/var/nix/db/db.sqlite
  case "$store" in
    local\?root=*)
      root=''${store#local?root=}
      root=''${root%/}
      [ -n "$root" ] || root=/
      store_dir=$root/nix/store
      db_path=$root/nix/var/nix/db/db.sqlite
      ;;
  esac
  [ "$store_dir" = "//nix/store" ] && store_dir=/nix/store

  # The scratch store is removed on exit unless --keep-scratch was passed, so
  # refuse anything that could take the real store (or a parent of it) with it.
  case "$scratch" in
    /|/nix|/nix/store|"$store_dir"|"$store_dir"/*)
      echo "$TOOL: refusing to use '$scratch' as the scratch store" >&2
      exit 64
      ;;
  esac

  work=$($MKTEMP -d)
  cleanup() {
    $RM -rf "$work"
    $RM -rf "$store_dir"/.cang-nix-store-repair.$$.*
    if [ "$keep_scratch" = 0 ] && [ -d "$scratch" ]; then
      $RM -rf "$scratch"
    fi
  }
  trap cleanup EXIT

  # The DB is the authority for validity, and reading it directly is orders of
  # magnitude cheaper than `nix path-info --all` (11 ms versus 4.8 s for a
  # 1.8k-path guest store), which matters because nix prep runs this on every
  # boot. Fall back to the CLI when the DB is not readable.
  valid_paths() {
    if [ -f "$db_path" ] && $SQLITE -cmd ".timeout 10000" "$db_path" \
        "select path from ValidPaths" > "$work/valid_raw" 2>/dev/null; then
      $SORT -u "$work/valid_raw" > "$work/valid"
      return 0
    fi
    $NIX path-info --store "$store" --all | $SORT -u > "$work/valid"
  }

  collect() {
    $FIND "$store_dir" -mindepth 1 -maxdepth 1 ! -name .links ! -name '*.lock' -print > "$work/present_raw" 2>/dev/null
    $SED -e "s|^$store_dir/|$logical/|" "$work/present_raw" \
      | $GREP -E "^$logical/[0-9a-df-np-sv-z]{32}-.+" | $SORT -u > "$work/present"
    valid_paths
    $COMM -23 "$work/valid" "$work/present" > "$work/phantoms"
    $COMM -13 "$work/valid" "$work/present" > "$work/orphans"
  }

  # The cang host seeds a new overlay generation by snapshotting the previous
  # upper (which keeps every cached store object) and installing the new image's
  # DB. The previous generation's registrations are parked next to it so this
  # also keeps those objects valid without refetching them: a union of the two
  # DBs, with the schema Nix itself uses.
  import_previous_db() {
    import_path=$1
    if [ ! -f "$import_path" ]; then
      echo "$TOOL: no previous-generation DB at '$import_path'" >&2
      return 1
    fi
    if [ ! -f "$db_path" ]; then
      echo "$TOOL: no DB to import into at '$db_path'" >&2
      return 1
    fi
    before=$($SQLITE -cmd ".timeout 10000" "$db_path" "select count(*) from ValidPaths")
    if ! $SQLITE -cmd ".timeout 10000" "$db_path" "
      attach database '$import_path' as previous;
      create temp table idmap (previous_id integer primary key, merged_id integer not null);
      insert into ValidPaths (path, hash, registrationTime, deriver, narSize, ultimate, sigs, ca)
        select path, hash, registrationTime, deriver, narSize, ultimate, sigs, ca
          from previous.ValidPaths
         where path not in (select path from main.ValidPaths);
      insert into idmap (previous_id, merged_id)
        select p.id, m.id from previous.ValidPaths p join main.ValidPaths m on m.path = p.path;
      insert into Refs (referrer, reference)
        select a.merged_id, b.merged_id
          from previous.Refs r
          join idmap a on a.previous_id = r.referrer
          join idmap b on b.previous_id = r.reference
         where not exists (select 1 from Refs x
                            where x.referrer = a.merged_id and x.reference = b.merged_id);
      insert into DerivationOutputs (drv, id, path)
        select m.merged_id, d.id, d.path
          from previous.DerivationOutputs d join idmap m on m.previous_id = d.drv
         where not exists (select 1 from DerivationOutputs x
                            where x.drv = m.merged_id and x.id = d.id);
      detach database previous;
    " >&2; then
      echo "$TOOL: failed to merge '$import_path' into '$db_path'" >&2
      return 1
    fi
    after=$($SQLITE -cmd ".timeout 10000" "$db_path" "select count(*) from ValidPaths")
    echo "$TOOL: merged previous generation DB: $before -> $after registered path(s)" >&2
  }

  if [ -n "$import_db" ]; then
    if ! import_previous_db "$import_db"; then
      echo "$TOOL: continuing without the previous generation's registrations" >&2
    fi
  fi

  if ! collect; then
    echo "$TOOL: failed to read the store validity metadata from '$store'" >&2
    exit 69
  fi
  if [ ! -s "$work/valid" ]; then
    echo "$TOOL: the store reported no valid paths; refusing to treat that as drift" >&2
    exit 69
  fi

  phantom_count=$($WC -l < "$work/phantoms" | $SED -e 's/ //g')
  orphan_count=$($WC -l < "$work/orphans" | $SED -e 's/ //g')
  echo "$TOOL: store=$store store-dir=$store_dir registered-but-absent=$phantom_count present-but-unregistered=$orphan_count"

  if [ "$orphan_count" -gt 0 ]; then
    echo "$TOOL: present-but-unregistered paths are inert; Nix reclaims them when it substitutes over them" >&2
    $HEAD -n 10 "$work/orphans" | $SED -e 's/^/  orphan: /' >&2
    if [ "$orphan_count" -gt 10 ]; then
      echo "  orphan: ... $orphan_count total" >&2
    fi
  fi

  if [ "$phantom_count" -eq 0 ]; then
    echo "$TOOL: ok - every path in the Nix DB has its store object"
    exit 0
  fi

  if [ "$dry_run" = 1 ]; then
    $SED -e 's/^/  absent: /' "$work/phantoms" >&2
    echo "$TOOL: dry run - $phantom_count path(s) would be repaired"
    exit 0
  fi

  substituters=''${CANG_NIX_REPAIR_SUBSTITUTERS:-}
  if [ -z "$substituters" ]; then
    substituters=$($NIX config show substituters 2>/dev/null \
      | $SED -e 's/^[^=]*=[[:space:]]*//' \
      | $TR ' ' '\n' | $GREP -E '^[a-z+]+://' | $SORT -u | $TR '\n' ' ')
  fi
  [ -n "$substituters" ] || substituters=https://cache.nixos.org

  sig_opt=
  if [ "''${CANG_NIX_REPAIR_NO_CHECK_SIGS:-}" = 1 ]; then
    sig_opt=--no-check-sigs
  fi

  fetch_into_scratch() {
    fetch_path=$1
    fetch_name=''${fetch_path#$logical/}
    if [ -e "$scratch/nix/store/$fetch_name" ]; then
      return 0
    fi
    for uri in $substituters; do
      [ -n "$uri" ] || continue
      echo "$TOOL: fetching $fetch_path from $uri" >&2
      if $NIX copy --to "local?root=$scratch" --from "$uri" $sig_opt \
        --option connect-timeout 5 --option download-attempts 2 "$fetch_path" >&2; then
        if [ -e "$scratch/nix/store/$fetch_name" ]; then
          return 0
        fi
      fi
    done
    return 1
  }

  restore_object() {
    restore_path=$1
    restore_name=''${restore_path#$logical/}
    restore_phys=$store_dir/$restore_name
    if [ -e "$restore_phys" ]; then
      return 0
    fi
    if ! fetch_into_scratch "$restore_path"; then
      return 1
    fi
    # Copy under a dot name and rename, so an interrupted run cannot leave a
    # half-written object where the DB claims a complete one.
    restore_tmp=$store_dir/.cang-nix-store-repair.$$.$restore_name
    if ! $CP -a "$scratch/nix/store/$restore_name" "$restore_tmp" >&2; then
      $RM -rf "$restore_tmp"
      return 1
    fi
    if ! $MV "$restore_tmp" "$restore_phys" >&2; then
      $RM -rf "$restore_tmp"
      return 1
    fi
    if $NIX_STORE --store "$store" --verify-path "$restore_path" >/dev/null 2>&1; then
      echo "$TOOL: restored $restore_path" >&2
      return 0
    fi
    echo "$TOOL: restored object for $restore_path but its narHash does not match the DB row" >&2
    $RM -rf "$restore_phys"
    return 1
  }

  deregister() {
    deregister_path=$1
    if [ ! -f "$db_path" ]; then
      return 1
    fi
    $SQLITE -cmd ".timeout 10000" "$db_path" "
      delete from Refs
        where referrer in (select id from ValidPaths where path = '$deregister_path')
           or reference in (select id from ValidPaths where path = '$deregister_path');
      delete from DerivationOutputs
        where drv in (select id from ValidPaths where path = '$deregister_path');
      delete from ValidPaths where path = '$deregister_path';
    " >&2
  }

  replain() {
    repair_path=$1
    for uri in $substituters; do
      [ -n "$uri" ] || continue
      echo "$TOOL: substituting $repair_path from $uri" >&2
      if $NIX copy --store "$store" --from "$uri" $sig_opt \
        --option connect-timeout 5 --option download-attempts 2 "$repair_path" >&2; then
        if $NIX_STORE --store "$store" --verify-path "$repair_path" >/dev/null 2>&1; then
          echo "$TOOL: re-substituted $repair_path" >&2
          return 0
        fi
      fi
    done
    return 1
  }

  repaired=0
  deregistered=0
  failed=0
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    if [ "$purge_only" = 0 ] && restore_object "$path"; then
      repaired=$((repaired + 1))
      continue
    fi
    if deregister "$path"; then
      # --purge-only never touches the network: after a seeded generation an
      # absent object means "the old image supplied it", and re-fetching the .drv
      # of every path that went with that image is exactly the download the seed
      # exists to avoid.
      if [ "$purge_only" = 0 ] && replain "$path"; then
        repaired=$((repaired + 1))
      else
        deregistered=$((deregistered + 1))
        echo "$TOOL: deregistered $path; Nix substitutes or rebuilds it on next use" >&2
      fi
    else
      failed=$((failed + 1))
      echo "$TOOL: cannot repair $path; no substituter offered it and its DB row is not writable" >&2
    fi
  done < "$work/phantoms"

  if ! collect; then
    echo "$TOOL: repaired $repaired path(s) but could not re-read the store metadata" >&2
    exit 1
  fi
  remaining=$($WC -l < "$work/phantoms" | $SED -e 's/ //g')

  echo "$TOOL: restored=$repaired deregistered=$deregistered unresolved=$failed"
  if [ "$remaining" -ne 0 ]; then
    $SED -e 's/^/  absent: /' "$work/phantoms" >&2
    echo "$TOOL: $remaining registered-but-absent path(s) remain" >&2
    exit 1
  fi
  echo "$TOOL: ok - every path in the Nix DB has its store object"
  exit 0
''
