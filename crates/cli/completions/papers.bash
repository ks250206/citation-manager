_papers() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
    prev="$3"
    cmd=""
    opts=""

    for i in "${COMP_WORDS[@]:0:COMP_CWORD}"
    do
        case "${cmd},${i}" in
            ",$1")
                cmd="papers"
                ;;
            papers,add)
                cmd="papers__subcmd__add"
                ;;
            papers,annotations)
                cmd="papers__subcmd__annotations"
                ;;
            papers,completions)
                cmd="papers__subcmd__completions"
                ;;
            papers,export)
                cmd="papers__subcmd__export"
                ;;
            papers,fulltext)
                cmd="papers__subcmd__fulltext"
                ;;
            papers,help)
                cmd="papers__subcmd__help"
                ;;
            papers,import)
                cmd="papers__subcmd__import"
                ;;
            papers,list)
                cmd="papers__subcmd__list"
                ;;
            papers,mcp)
                cmd="papers__subcmd__mcp"
                ;;
            papers,search)
                cmd="papers__subcmd__search"
                ;;
            papers,server)
                cmd="papers__subcmd__server"
                ;;
            papers,show)
                cmd="papers__subcmd__show"
                ;;
            papers__subcmd__add,doi)
                cmd="papers__subcmd__add__subcmd__doi"
                ;;
            papers__subcmd__add,file)
                cmd="papers__subcmd__add__subcmd__file"
                ;;
            papers__subcmd__add,help)
                cmd="papers__subcmd__add__subcmd__help"
                ;;
            papers__subcmd__add__subcmd__help,doi)
                cmd="papers__subcmd__add__subcmd__help__subcmd__doi"
                ;;
            papers__subcmd__add__subcmd__help,file)
                cmd="papers__subcmd__add__subcmd__help__subcmd__file"
                ;;
            papers__subcmd__add__subcmd__help,help)
                cmd="papers__subcmd__add__subcmd__help__subcmd__help"
                ;;
            papers__subcmd__help,add)
                cmd="papers__subcmd__help__subcmd__add"
                ;;
            papers__subcmd__help,annotations)
                cmd="papers__subcmd__help__subcmd__annotations"
                ;;
            papers__subcmd__help,completions)
                cmd="papers__subcmd__help__subcmd__completions"
                ;;
            papers__subcmd__help,export)
                cmd="papers__subcmd__help__subcmd__export"
                ;;
            papers__subcmd__help,fulltext)
                cmd="papers__subcmd__help__subcmd__fulltext"
                ;;
            papers__subcmd__help,help)
                cmd="papers__subcmd__help__subcmd__help"
                ;;
            papers__subcmd__help,import)
                cmd="papers__subcmd__help__subcmd__import"
                ;;
            papers__subcmd__help,list)
                cmd="papers__subcmd__help__subcmd__list"
                ;;
            papers__subcmd__help,mcp)
                cmd="papers__subcmd__help__subcmd__mcp"
                ;;
            papers__subcmd__help,search)
                cmd="papers__subcmd__help__subcmd__search"
                ;;
            papers__subcmd__help,server)
                cmd="papers__subcmd__help__subcmd__server"
                ;;
            papers__subcmd__help,show)
                cmd="papers__subcmd__help__subcmd__show"
                ;;
            papers__subcmd__help__subcmd__add,doi)
                cmd="papers__subcmd__help__subcmd__add__subcmd__doi"
                ;;
            papers__subcmd__help__subcmd__add,file)
                cmd="papers__subcmd__help__subcmd__add__subcmd__file"
                ;;
            papers__subcmd__help__subcmd__server,start)
                cmd="papers__subcmd__help__subcmd__server__subcmd__start"
                ;;
            papers__subcmd__help__subcmd__server,status)
                cmd="papers__subcmd__help__subcmd__server__subcmd__status"
                ;;
            papers__subcmd__help__subcmd__server,stop)
                cmd="papers__subcmd__help__subcmd__server__subcmd__stop"
                ;;
            papers__subcmd__server,help)
                cmd="papers__subcmd__server__subcmd__help"
                ;;
            papers__subcmd__server,start)
                cmd="papers__subcmd__server__subcmd__start"
                ;;
            papers__subcmd__server,status)
                cmd="papers__subcmd__server__subcmd__status"
                ;;
            papers__subcmd__server,stop)
                cmd="papers__subcmd__server__subcmd__stop"
                ;;
            papers__subcmd__server__subcmd__help,help)
                cmd="papers__subcmd__server__subcmd__help__subcmd__help"
                ;;
            papers__subcmd__server__subcmd__help,start)
                cmd="papers__subcmd__server__subcmd__help__subcmd__start"
                ;;
            papers__subcmd__server__subcmd__help,status)
                cmd="papers__subcmd__server__subcmd__help__subcmd__status"
                ;;
            papers__subcmd__server__subcmd__help,stop)
                cmd="papers__subcmd__server__subcmd__help__subcmd__stop"
                ;;
            *)
                ;;
        esac
    done

    case "${cmd}" in
        papers)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version add import search list show fulltext annotations export server mcp completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version doi file help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__doi)
            opts="-y -h -V --title --yes --dry-run --on-duplicate --merge --merge-fields --collection --tag --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --title)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --on-duplicate)
                    COMPREPLY=($(compgen -W "ask skip create" -- "${cur}"))
                    return 0
                    ;;
                --merge)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --merge-fields)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --tag)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__file)
            opts="-y -h -V --attach-to --title --doi --yes --dry-run --on-duplicate --merge --merge-fields --collection --tag --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --attach-to)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --title)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --doi)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --on-duplicate)
                    COMPREPLY=($(compgen -W "ask skip create" -- "${cur}"))
                    return 0
                    ;;
                --merge)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --merge-fields)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --tag)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__help)
            opts="doi file help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__help__subcmd__doi)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__help__subcmd__file)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__add__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__annotations)
            opts="-h -V --attachment --limit --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --attachment)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__completions)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version bash zsh fish"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__export)
            opts="-o -h -V --format --ids --collection --output --force --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --format)
                    COMPREPLY=($(compgen -W "bibtex ris" -- "${cur}"))
                    return 0
                    ;;
                --ids)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --output)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -o)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__fulltext)
            opts="-h -V --page --pages --max-pages --max-chars --attachment --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --page)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --pages)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --max-pages)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --max-chars)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --attachment)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help)
            opts="add import search list show fulltext annotations export server mcp completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__add)
            opts="doi file"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__add__subcmd__doi)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__add__subcmd__file)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__annotations)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__completions)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__export)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__fulltext)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__import)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__mcp)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__search)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__server)
            opts="status start stop"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__server__subcmd__start)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__server__subcmd__status)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__server__subcmd__stop)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__import)
            opts="-y -h -V --format --batch --yes --dry-run --on-duplicate --collection --tag --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --format)
                    COMPREPLY=($(compgen -W "bibtex ris" -- "${cur}"))
                    return 0
                    ;;
                --batch)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --on-duplicate)
                    COMPREPLY=($(compgen -W "ask skip create" -- "${cur}"))
                    return 0
                    ;;
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --tag)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__list)
            opts="-h -V --collection --tag --any-tag --keyword --author --year-from --year-to --has-pdf --sort --desc --limit --cursor --trash --include-trash --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --tag)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --keyword)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --author)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --year-from)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --year-to)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --has-pdf)
                    COMPREPLY=($(compgen -W "true false" -- "${cur}"))
                    return 0
                    ;;
                --sort)
                    COMPREPLY=($(compgen -W "id title year created updated" -- "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --cursor)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__mcp)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__search)
            opts="-h -V --tag --collection --sources --limit --cursor --snippet-chars --all-pages --include-trash --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --tag)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --collection)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --sources)
                    COMPREPLY=($(compgen -W "bibliographic annotations pages" -- "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --cursor)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --snippet-chars)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version status start stop help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__help)
            opts="status start stop help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__help__subcmd__start)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__help__subcmd__status)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__help__subcmd__stop)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__start)
            opts="-h -V --rest --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__status)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__server__subcmd__stop)
            opts="-h -V --force --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        papers__subcmd__show)
            opts="-h -V --library --runtime-dir --json --text --read-only --credential-file --no-start --daemon-idle-timeout-ms --help --version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --library)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --runtime-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --credential-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --daemon-idle-timeout-ms)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _papers -o nosort -o bashdefault -o default papers
else
    complete -F _papers -o bashdefault -o default papers
fi
