# bash completion for lyricslens and lls

_lyricslens() {
    local current=${COMP_WORDS[COMP_CWORD]}
    # The program takes options only, never a file.
    COMPREPLY=($(compgen -W "--help --version --foreground --settings --toggle --song --position --sync --quit --paths --upgrade --uninstall" -- "$current"))
}

complete -F _lyricslens lyricslens lls
