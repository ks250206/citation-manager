# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_papers_global_optspecs
    string join \n library= runtime-dir= json text read-only credential-file= no-start daemon-idle-timeout-ms= h/help V/version
end

function __fish_papers_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_papers_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_papers_using_subcommand
    set -l cmd (__fish_papers_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c papers -n "__fish_papers_needs_command" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_needs_command" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_needs_command" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_needs_command" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_needs_command" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_needs_command" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_needs_command" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_needs_command" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_needs_command" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_needs_command" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_needs_command" -f -a "add" -d 'Register a paper by DOI or from a local PDF'
complete -c papers -n "__fish_papers_needs_command" -f -a "import" -d 'Import a BibTeX or RIS file (preview, duplicate review, then commit)'
complete -c papers -n "__fish_papers_needs_command" -f -a "search" -d 'Full-text search over bibliography, annotations and extracted PDF text'
complete -c papers -n "__fish_papers_needs_command" -f -a "list" -d 'List papers with filters, sort and keyset paging'
complete -c papers -n "__fish_papers_needs_command" -f -a "show" -d 'Show one paper with its collections, tags and attachments'
complete -c papers -n "__fish_papers_needs_command" -f -a "fulltext" -d 'Read extracted PDF page text within page and character limits'
complete -c papers -n "__fish_papers_needs_command" -f -a "annotations" -d 'List a paper\'s highlights, comments and notes'
complete -c papers -n "__fish_papers_needs_command" -f -a "export" -d 'Export BibTeX/RIS to a file or stdout'
complete -c papers -n "__fish_papers_needs_command" -f -a "server" -d 'Inspect, start or stop the library\'s papersd'
complete -c papers -n "__fish_papers_needs_command" -f -a "mcp" -d 'Serve MCP over stdio (requires the #29 MCP server; unavailable until then)'
complete -c papers -n "__fish_papers_needs_command" -f -a "completions" -d 'Print a shell completion script'
complete -c papers -n "__fish_papers_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -f -a "doi" -d 'Look up a DOI (Crossref/DataCite), preview duplicates, then commit'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -f -a "file" -d 'Register a local PDF as a new paper, or attach it to an existing paper'
complete -c papers -n "__fish_papers_using_subcommand add; and not __fish_seen_subcommand_from doi file help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l title -d 'Title to use when the lookup has none (or to override it)' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l on-duplicate -d 'What to do with a duplicate candidate. Merging always needs --merge' -r -f -a "ask\t'Stop with exit 4 and show the candidates (default)'
skip\t'Skip rows that match an existing paper (exactly or approximately)'
create\t'Create a separate paper when the match is only approximate. Exact identifier matches (DOI/arXiv/PMID) can never be created twice; they still need skip or `--merge`'"
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l merge -d 'Merge into this reviewed duplicate candidate instead of creating a paper' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l merge-fields -d 'Fields taken from the incoming record on --merge: none (default), all, or a list' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l collection -d 'Add the new paper to this existing collection (name or ID)' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l tag -d 'Add an existing tag (name or ID); repeatable' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -s y -l yes -d 'Commit without an interactive prompt (required when stdin is not a terminal)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l dry-run -d 'Preview only: report what would happen and save nothing'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from doi" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l attach-to -d 'Attach to this existing paper; its metadata is never changed by the file' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l title -d 'Explicit title (otherwise PDF metadata, else a provisional title from the filename)' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l doi -d 'Explicit DOI for the new paper' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l on-duplicate -d 'What to do with a duplicate candidate. Merging always needs --merge' -r -f -a "ask\t'Stop with exit 4 and show the candidates (default)'
skip\t'Skip rows that match an existing paper (exactly or approximately)'
create\t'Create a separate paper when the match is only approximate. Exact identifier matches (DOI/arXiv/PMID) can never be created twice; they still need skip or `--merge`'"
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l merge -d 'Merge into this reviewed duplicate candidate instead of creating a paper' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l merge-fields -d 'Fields taken from the incoming record on --merge: none (default), all, or a list' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l collection -d 'Add the new paper to this existing collection (name or ID)' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l tag -d 'Add an existing tag (name or ID); repeatable' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -s y -l yes -d 'Commit without an interactive prompt (required when stdin is not a terminal)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l dry-run -d 'Preview only: report what would happen and save nothing'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from file" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from help" -f -a "doi" -d 'Look up a DOI (Crossref/DataCite), preview duplicates, then commit'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from help" -f -a "file" -d 'Register a local PDF as a new paper, or attach it to an existing paper'
complete -c papers -n "__fish_papers_using_subcommand add; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand import" -l format -d 'Input format (default: from the file extension)' -r -f -a "bibtex\t''
ris\t''"
complete -c papers -n "__fish_papers_using_subcommand import" -l batch -d 'Rows per preview/commit batch' -r
complete -c papers -n "__fish_papers_using_subcommand import" -l on-duplicate -d 'What to do with a duplicate candidate. Merging always needs --merge' -r -f -a "ask\t'Stop with exit 4 and show the candidates (default)'
skip\t'Skip rows that match an existing paper (exactly or approximately)'
create\t'Create a separate paper when the match is only approximate. Exact identifier matches (DOI/arXiv/PMID) can never be created twice; they still need skip or `--merge`'"
complete -c papers -n "__fish_papers_using_subcommand import" -l collection -d 'Add the new paper to this existing collection (name or ID)' -r
complete -c papers -n "__fish_papers_using_subcommand import" -l tag -d 'Add an existing tag (name or ID); repeatable' -r
complete -c papers -n "__fish_papers_using_subcommand import" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand import" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand import" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand import" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand import" -s y -l yes -d 'Commit without an interactive prompt (required when stdin is not a terminal)'
complete -c papers -n "__fish_papers_using_subcommand import" -l dry-run -d 'Preview only: report what would happen and save nothing'
complete -c papers -n "__fish_papers_using_subcommand import" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand import" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand import" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand import" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand import" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c papers -n "__fish_papers_using_subcommand import" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand search" -l tag -d 'Require this tag (adds tag:"..." to the query); repeatable' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l collection -d 'Restrict to a collection (ID, or name added as collection:"...")' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l sources -d 'Sources to search (default: all)' -r -f -a "bibliographic\t''
annotations\t''
pages\t''"
complete -c papers -n "__fish_papers_using_subcommand search" -l limit -d 'Maximum hits (1-100)' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l cursor -d 'Continue from a previous `next_cursor`/`continuation`' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l snippet-chars -d 'Snippet length in characters (32-1000)' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand search" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand search" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand search" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand search" -l all-pages -d 'Scan PDF pages for 1-2 character terms without a narrowing filter (bounded windows)'
complete -c papers -n "__fish_papers_using_subcommand search" -l include-trash -d 'Include papers in the trash'
complete -c papers -n "__fish_papers_using_subcommand search" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand search" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand search" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand search" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand search" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand search" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand list" -l collection -d 'Direct members of this collection (name or ID)' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l tag -d 'Tag filter (name or ID); repeatable' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l keyword -d 'Literal substring filter over bibliographic fields' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l author -r
complete -c papers -n "__fish_papers_using_subcommand list" -l year-from -r
complete -c papers -n "__fish_papers_using_subcommand list" -l year-to -r
complete -c papers -n "__fish_papers_using_subcommand list" -l has-pdf -d 'Only papers with (true) or without (false) a PDF attachment' -r -f -a "true\t''
false\t''"
complete -c papers -n "__fish_papers_using_subcommand list" -l sort -r -f -a "id\t''
title\t''
year\t''
created\t''
updated\t''"
complete -c papers -n "__fish_papers_using_subcommand list" -l limit -d 'Page size (1-500)' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l cursor -d 'Continue after a previous `next_cursor`' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand list" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand list" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand list" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand list" -l any-tag -d 'Match any instead of all of the given tags'
complete -c papers -n "__fish_papers_using_subcommand list" -l desc
complete -c papers -n "__fish_papers_using_subcommand list" -l trash -d 'List papers in the trash instead of active ones'
complete -c papers -n "__fish_papers_using_subcommand list" -l include-trash -d 'Include trashed papers'
complete -c papers -n "__fish_papers_using_subcommand list" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand list" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand list" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand list" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand list" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand list" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand show" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand show" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand show" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand show" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand show" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand show" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand show" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand show" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand show" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand show" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l page -d 'One page by its 1-based physical number' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l pages -d 'Page range, 1-based inclusive (e.g. 3-7), at most 50 pages' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l max-pages -d 'Pages to return when no --page/--pages is given (1-50)' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l max-chars -d 'Total characters returned (1-400000)' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l attachment -d 'Which PDF attachment (default: the paper\'s first)' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand fulltext" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand annotations" -l attachment -d 'Only this attachment\'s annotations' -r
complete -c papers -n "__fish_papers_using_subcommand annotations" -l limit -d 'Maximum annotations returned (1-1000)' -r
complete -c papers -n "__fish_papers_using_subcommand annotations" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand annotations" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand annotations" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand annotations" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand annotations" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand annotations" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand annotations" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand annotations" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand annotations" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand annotations" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand export" -l format -r -f -a "bibtex\t''
ris\t''"
complete -c papers -n "__fish_papers_using_subcommand export" -l ids -d 'Comma-separated paper IDs' -r
complete -c papers -n "__fish_papers_using_subcommand export" -l collection -d 'Direct members of a collection (name or ID)' -r
complete -c papers -n "__fish_papers_using_subcommand export" -s o -l output -d 'Destination file (outside the library). Without it the bibliography goes to stdout' -r -F
complete -c papers -n "__fish_papers_using_subcommand export" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand export" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand export" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand export" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand export" -l force -d 'Replace an existing regular file at --output'
complete -c papers -n "__fish_papers_using_subcommand export" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand export" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand export" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand export" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand export" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand export" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -f -a "status" -d 'Report whether papersd serves this library (never starts it)'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -f -a "start" -d 'Start papersd now (it exits by itself once idle)'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -f -a "stop" -d 'Stop papersd (SIGTERM). Refuses while other clients or jobs are active unless --force'
complete -c papers -n "__fish_papers_using_subcommand server; and not __fish_seen_subcommand_from status start stop help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from status" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l rest -d 'REST API listener (not available yet: #27)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from start" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l force
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from stop" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from help" -f -a "status" -d 'Report whether papersd serves this library (never starts it)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from help" -f -a "start" -d 'Start papersd now (it exits by itself once idle)'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from help" -f -a "stop" -d 'Stop papersd (SIGTERM). Refuses while other clients or jobs are active unless --force'
complete -c papers -n "__fish_papers_using_subcommand server; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand mcp" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand mcp" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand mcp" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand mcp" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand mcp" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand mcp" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand mcp" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand mcp" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand mcp" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand mcp" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand completions" -l library -d 'Library directory (absolute or relative). Required for library commands' -r -F
complete -c papers -n "__fish_papers_using_subcommand completions" -l runtime-dir -d 'Private runtime directory for the daemon socket (default: $XDG_RUNTIME_DIR/citation-manager)' -r -F
complete -c papers -n "__fish_papers_using_subcommand completions" -l credential-file -d 'Present a credential issued by the daemon (0600 file) instead of the owner\'s desktop credential' -r -F
complete -c papers -n "__fish_papers_using_subcommand completions" -l daemon-idle-timeout-ms -d 'Idle timeout passed to a papersd that this invocation starts' -r
complete -c papers -n "__fish_papers_using_subcommand completions" -l json -d 'Emit the JSON envelope on stdout (default)'
complete -c papers -n "__fish_papers_using_subcommand completions" -l text -d 'Emit human-readable text on stdout instead of JSON'
complete -c papers -n "__fish_papers_using_subcommand completions" -l read-only -d 'Ask the daemon for LibraryRead only, whatever the command; writes are then refused (exit 5)'
complete -c papers -n "__fish_papers_using_subcommand completions" -l no-start -d 'Never start papersd; fail with exit 6 when it is not running'
complete -c papers -n "__fish_papers_using_subcommand completions" -s h -l help -d 'Print help'
complete -c papers -n "__fish_papers_using_subcommand completions" -s V -l version -d 'Print version'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "add" -d 'Register a paper by DOI or from a local PDF'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "import" -d 'Import a BibTeX or RIS file (preview, duplicate review, then commit)'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "search" -d 'Full-text search over bibliography, annotations and extracted PDF text'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "list" -d 'List papers with filters, sort and keyset paging'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "show" -d 'Show one paper with its collections, tags and attachments'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "fulltext" -d 'Read extracted PDF page text within page and character limits'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "annotations" -d 'List a paper\'s highlights, comments and notes'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "export" -d 'Export BibTeX/RIS to a file or stdout'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "server" -d 'Inspect, start or stop the library\'s papersd'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "mcp" -d 'Serve MCP over stdio (requires the #29 MCP server; unavailable until then)'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "completions" -d 'Print a shell completion script'
complete -c papers -n "__fish_papers_using_subcommand help; and not __fish_seen_subcommand_from add import search list show fulltext annotations export server mcp completions help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c papers -n "__fish_papers_using_subcommand help; and __fish_seen_subcommand_from add" -f -a "doi" -d 'Look up a DOI (Crossref/DataCite), preview duplicates, then commit'
complete -c papers -n "__fish_papers_using_subcommand help; and __fish_seen_subcommand_from add" -f -a "file" -d 'Register a local PDF as a new paper, or attach it to an existing paper'
complete -c papers -n "__fish_papers_using_subcommand help; and __fish_seen_subcommand_from server" -f -a "status" -d 'Report whether papersd serves this library (never starts it)'
complete -c papers -n "__fish_papers_using_subcommand help; and __fish_seen_subcommand_from server" -f -a "start" -d 'Start papersd now (it exits by itself once idle)'
complete -c papers -n "__fish_papers_using_subcommand help; and __fish_seen_subcommand_from server" -f -a "stop" -d 'Stop papersd (SIGTERM). Refuses while other clients or jobs are active unless --force'
