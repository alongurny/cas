# Completions of cas for fish, printed by `cas completions fish`.
function __cas_command
    set -l words (commandline -opc)
    set -e words[1]
    set -l skip 0
    for w in $words
        if test $skip -eq 1
            set skip 0
            continue
        end
        switch $w
            case --store --token-file --ca --meta -j -H -n --author --notes --annotate --as --tag --change --changelog --cert --key --max-body --project
                set skip 1
            case '-*'
            case '*'
                echo $w
                return 0
        end
    end
    return 1
end
function __cas_no_command
    not __cas_command >/dev/null
end
function __cas_using
    test (__cas_command) = $argv[1] 2>/dev/null
end
function __cas_histories
    command cas __histories 2>/dev/null
end
complete -c cas -f
complete -c cas -n __cas_no_command -a commit -d 'add <dir> as a new commit after the tip'
complete -c cas -n __cas_no_command -a status -d 'how <dir> differs from the commit'
complete -c cas -n __cas_no_command -a reset -d 'make <dir> the commit\'s version again'
complete -c cas -n __cas_no_command -a clean -d 'remove from <dir> the paths the commit does not have'
complete -c cas -n __cas_no_command -a stash -d 'set the changes in <dir> aside, or bring them back'
complete -c cas -n __cas_no_command -a watch -d 'follow the changes in <dir> until stopped'
complete -c cas -n __cas_no_command -a checkout -d 'write the commit\'s version, or the part under <prefix>, to <dest>'
complete -c cas -n __cas_no_command -a sparse -d 'write only the named files and directories of the commit to <dest>'
complete -c cas -n __cas_no_command -a log -d 'the commits, newest first'
complete -c cas -n __cas_no_command -a ls -d 'the paths of the commit\'s version'
complete -c cas -n __cas_no_command -a diff -d 'what the commit changed'
complete -c cas -n __cas_no_command -a notes -d 'the notes of the commit'
complete -c cas -n __cas_no_command -a summary -d 'how many directories and files the commit\'s version has'
complete -c cas -n __cas_no_command -a path-log -d 'the commits that changed <path>'
complete -c cas -n __cas_no_command -a last-modified -d 'each entry under <prefix> with the commit that last changed it'
complete -c cas -n __cas_no_command -a merge-base -d 'the best common ancestor of commits <a> and <b>'
complete -c cas -n __cas_no_command -a merge -d 'merge history <other> into this one as one commit'
complete -c cas -n __cas_no_command -a rebase -d 'replay this history\'s own commits onto history <onto>'
complete -c cas -n __cas_no_command -a push -d 'send the history to another store'
complete -c cas -n __cas_no_command -a pull -d 'fetch the history from another store'
complete -c cas -n __cas_no_command -a publish -d 'add the commit\'s version to the release history as a release'
complete -c cas -n __cas_no_command -a compatible -d 'whether upgrading from release <from> to <to> is safe'
complete -c cas -n __cas_no_command -a tag -d 'name a node of a history in the earlier format'
complete -c cas -n __cas_no_command -a import-git -d 'import the commits of a git repository as a new history'
complete -c cas -n __cas_no_command -a gc -d 'remove blobs that no history reaches'
complete -c cas -n __cas_no_command -a fsck -d 'check every reachable blob: readable, valid and complete'
complete -c cas -n __cas_no_command -a objects -d 'every blob key in the store'
complete -c cas -n __cas_no_command -a show -d 'print a blob'
complete -c cas -n __cas_no_command -a annotated -d 'store or print a text with a note on each line'
complete -c cas -n __cas_no_command -a serve -d 'serve the store over HTTPS'
complete -c cas -n __cas_no_command -a access -d 'the access policy of a served store'
complete -c cas -n __cas_no_command -a signup -d 'make an account, sign in, or change the password'
complete -c cas -n __cas_no_command -a login -d 'make an account, sign in, or change the password'
complete -c cas -n __cas_no_command -a whoami -d 'make an account, sign in, or change the password'
complete -c cas -n __cas_no_command -a password -d 'make an account, sign in, or change the password'
complete -c cas -n __cas_no_command -a account -d 'show or edit an account'
complete -c cas -n __cas_no_command -a token -d 'the tokens of the account'
complete -c cas -n __cas_no_command -a org -d 'organizations and their members'
complete -c cas -n __cas_no_command -a projects -d 'the projects you can read'
complete -c cas -n __cas_no_command -a project -d 'make and manage a project'
complete -c cas -n __cas_no_command -a histories -d 'the project\'s histories and their tips'
complete -c cas -n __cas_no_command -a blame -d 'each line of <path> with the commit that last changed it'
complete -c cas -n __cas_no_command -a checks -d 'the check results of a commit'
complete -c cas -n __cas_no_command -a proposal -d 'propose, review and merge a change'
complete -c cas -n __cas_no_command -a issue -d 'issues of a project'
complete -c cas -n __cas_no_command -a release -d 'releases of a project'
complete -c cas -n __cas_no_command -a mirror -d 'mirror a git repository'
complete -c cas -n __cas_no_command -a events -d 'the event log of a project'
complete -c cas -n __cas_no_command -a help -d 'explain cas, or one command'
complete -c cas -n __cas_no_command -a completions -d 'print the completion script for a shell'
complete -c cas -n __cas_no_command -a version -d 'print the version of cas'
complete -c cas -l store -r -F -d 'the store: a directory (default $CAS_STORE, else .cas) or an https URL'
complete -c cas -l token-file -r -F -d 'the file holding the token for a store under access control'
complete -c cas -l ca -r -F -d 'trust the CA certificates in <file> instead of the public ones'
complete -c cas -l meta -r -F -d 'the store holding the access policy (default .cas-meta)'
complete -c cas -s j -x -d 'the most threads a command uses (default: one for each core)'
complete -c cas -s H -x -a '(__cas_histories)' -d 'the history to work on (default main)'
complete -c cas -s n -x -d 'a commit: latest, a key or its start; a tag on a release history (default latest)'
complete -c cas -l help -d 'explain the command instead of running it (also -h)'
complete -c cas -l version -d 'print the version of cas'
complete -c cas -n 'contains -- (__cas_command) commit stash merge' -l author -x -d 'who makes the commit (default $CAS_AUTHOR, else $USER)'
complete -c cas -n 'contains -- (__cas_command) commit' -l notes -r -F -d 'notes for the step: what `status <dir> --diff` prints, with `# note` lines added'
complete -c cas -n 'contains -- (__cas_command) commit' -l annotate -r -F -d 'notes on the file <path> as it is: its lines as `| line`, with `# note` lines'
complete -c cas -n 'contains -- (__cas_command) status' -l diff -d 'print the line diff, ready to add notes to'
complete -c cas -n 'contains -- (__cas_command) diff' -l annotated -d 'print the line diff with the notes of the step'
complete -c cas -n 'contains -- (__cas_command) push pull publish' -l as -x -a '(__cas_histories)' -d 'the name of the history on the other side, or of the release history'
complete -c cas -n 'contains -- (__cas_command) publish' -l tag -x -d 'the semantic version of the release, such as 1.4.0'
complete -c cas -n 'contains -- (__cas_command) publish' -l change -x -a 'breaking feature patch security' -d 'what the release changes since the one before: breaking, feature, patch or security'
complete -c cas -n 'contains -- (__cas_command) publish' -l changelog -r -F -d 'the text that says what the release changes'
complete -c cas -n 'contains -- (__cas_command) gc' -l full -d 'walk the whole store, not only what came since the last full run'
complete -c cas -n 'contains -- (__cas_command) serve' -l cert -r -F -d 'the PEM certificate chain to serve with'
complete -c cas -n 'contains -- (__cas_command) serve' -l key -r -F -d 'the PEM private key to serve with'
complete -c cas -n 'contains -- (__cas_command) serve' -l open -d 'serve to anyone, without an access policy'
complete -c cas -n 'contains -- (__cas_command) serve' -l max-body -x -d 'the largest request body accepted (default 64 MiB)'
complete -c cas -n 'contains -- (__cas_command) commit' -l nodes -d 'start a history in the earlier format, which the forge still keeps'
complete -c cas -l compatible -d 'on a history in the earlier format, mark the step compatible'
complete -c cas -l json -d 'print the forge\'s answer as JSON'
complete -c cas -l project -x -d 'the project on the forge, when --store does not name it'
complete -c cas -n '__cas_using commit' -F
complete -c cas -n '__cas_using status' -F
complete -c cas -n '__cas_using reset' -F
complete -c cas -n '__cas_using clean' -F
complete -c cas -n '__cas_using stash' -a 'pop'
complete -c cas -n '__cas_using stash' -F
complete -c cas -n '__cas_using watch' -F
complete -c cas -n '__cas_using checkout' -F
complete -c cas -n '__cas_using sparse' -F
complete -c cas -n '__cas_using push' -F
complete -c cas -n '__cas_using pull' -F
complete -c cas -n '__cas_using import-git' -F
complete -c cas -n '__cas_using annotated' -a 'make show'
complete -c cas -n '__cas_using annotated' -F
complete -c cas -n '__cas_using serve' -F
complete -c cas -n '__cas_using access' -a 'show user grant revoke'
complete -c cas -n '__cas_using account' -a 'edit'
complete -c cas -n '__cas_using token' -a 'list make revoke'
complete -c cas -n '__cas_using org' -a 'create member'
complete -c cas -n '__cas_using project' -a 'create show delete fork edit member protect gate label webhook runner'
complete -c cas -n '__cas_using proposal' -a 'list open show edit close reopen approve merge thread reply resolve promote'
complete -c cas -n '__cas_using issue' -a 'list open show comment edit close reopen label'
complete -c cas -n '__cas_using release' -a 'list publish archive'
complete -c cas -n '__cas_using mirror' -a 'add pull'
complete -c cas -n '__cas_using completions' -a 'fish bash zsh'
complete -c cas -n '__cas_using help' -a 'commit status reset clean stash watch checkout sparse log ls diff notes summary path-log last-modified merge-base merge rebase push pull publish compatible tag import-git gc fsck objects show annotated serve access signup login whoami password account token org projects project histories blame checks proposal issue release mirror events help completions version'
complete -c cas -n '__cas_using merge' -a '(__cas_histories)'
complete -c cas -n '__cas_using rebase' -a '(__cas_histories)'
