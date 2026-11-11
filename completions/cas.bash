# Completions of cas for bash, printed by `cas completions bash`.
_cas() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]} cmd= i skip=0
    for ((i = 1; i < COMP_CWORD; i++)); do
        if ((skip)); then skip=0; continue; fi
        case ${COMP_WORDS[i]} in
            --store|--token-file|--ca|--meta|-j|-H|-n|--author|--notes|--annotate|--as|--tag|--change|--changelog|--cert|--key|--max-body|--project) skip=1 ;;
            -*) ;;
            *) cmd=${COMP_WORDS[i]}; break ;;
        esac
    done
    case $prev in
        --store) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --token-file) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --ca) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --meta) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        -j) COMPREPLY=(); return ;;
        -H) COMPREPLY=($(compgen -W "$(command cas __histories 2>/dev/null)" -- "$cur")); return ;;
        -n) COMPREPLY=(); return ;;
        --author) COMPREPLY=(); return ;;
        --notes) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --annotate) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --as) COMPREPLY=($(compgen -W "$(command cas __histories 2>/dev/null)" -- "$cur")); return ;;
        --tag) COMPREPLY=(); return ;;
        --change) COMPREPLY=($(compgen -W 'breaking feature patch security' -- "$cur")); return ;;
        --changelog) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --cert) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --key) COMPREPLY=($(compgen -f -- "$cur")); return ;;
        --max-body) COMPREPLY=(); return ;;
        --project) COMPREPLY=(); return ;;
    esac
    if [[ $cur == -* ]]; then
        local opts='--store --token-file --ca --meta -j -H -n --help --version'
        case $cmd in
            commit) opts+=' --author --notes --annotate --nodes' ;;
            status) opts+=' --diff' ;;
            stash) opts+=' --author' ;;
            diff) opts+=' --annotated' ;;
            prove) opts+=' --all --quiet' ;;
            verify) opts+=' --all --quiet' ;;
            merge) opts+=' --author' ;;
            push) opts+=' --as' ;;
            pull) opts+=' --as' ;;
            publish) opts+=' --tag --change --changelog --as' ;;
            gc) opts+=' --full' ;;
            serve) opts+=' --cert --key --open --max-body' ;;
        esac
        COMPREPLY=($(compgen -W "$opts" -- "$cur"))
        return
    fi
    case $cmd in
        '') COMPREPLY=($(compgen -W 'commit status reset clean stash watch checkout sparse log ls diff notes manifest prove verify summary path-log last-modified merge-base merge rebase push pull publish compatible tag import-git gc fsck objects show annotated serve access signup login whoami password account token org projects project histories blame checks proposal issue release mirror events help completions version' -- "$cur")) ;;
        help) COMPREPLY=($(compgen -W 'commit status reset clean stash watch checkout sparse log ls diff notes manifest prove verify summary path-log last-modified merge-base merge rebase push pull publish compatible tag import-git gc fsck objects show annotated serve access signup login whoami password account token org projects project histories blame checks proposal issue release mirror events help completions version' -- "$cur")) ;;
        merge|rebase) COMPREPLY=($(compgen -W "$(command cas __histories 2>/dev/null)" -- "$cur")) ;;
        commit) COMPREPLY=($(compgen -f -- "$cur")) ;;
        status) COMPREPLY=($(compgen -f -- "$cur")) ;;
        reset) COMPREPLY=($(compgen -f -- "$cur")) ;;
        clean) COMPREPLY=($(compgen -f -- "$cur")) ;;
        stash) COMPREPLY=($(compgen -W 'pop' -- "$cur") $(compgen -f -- "$cur")) ;;
        watch) COMPREPLY=($(compgen -f -- "$cur")) ;;
        checkout) COMPREPLY=($(compgen -f -- "$cur")) ;;
        sparse) COMPREPLY=($(compgen -f -- "$cur")) ;;
        push) COMPREPLY=($(compgen -f -- "$cur")) ;;
        pull) COMPREPLY=($(compgen -f -- "$cur")) ;;
        import-git) COMPREPLY=($(compgen -f -- "$cur")) ;;
        annotated) COMPREPLY=($(compgen -W 'make show' -- "$cur") $(compgen -f -- "$cur")) ;;
        serve) COMPREPLY=($(compgen -f -- "$cur")) ;;
        access) COMPREPLY=($(compgen -W 'show user grant revoke' -- "$cur") ) ;;
        account) COMPREPLY=($(compgen -W 'edit' -- "$cur") ) ;;
        token) COMPREPLY=($(compgen -W 'list make revoke' -- "$cur") ) ;;
        org) COMPREPLY=($(compgen -W 'create member' -- "$cur") ) ;;
        project) COMPREPLY=($(compgen -W 'create show delete fork edit member protect gate label webhook runner' -- "$cur") ) ;;
        proposal) COMPREPLY=($(compgen -W 'list open show edit close reopen approve merge thread reply resolve promote' -- "$cur") ) ;;
        issue) COMPREPLY=($(compgen -W 'list open show comment edit close reopen label' -- "$cur") ) ;;
        release) COMPREPLY=($(compgen -W 'list publish archive' -- "$cur") ) ;;
        mirror) COMPREPLY=($(compgen -W 'add pull' -- "$cur") ) ;;
        completions) COMPREPLY=($(compgen -W 'fish bash zsh' -- "$cur") ) ;;
    esac
}
complete -o filenames -F _cas cas
