# fish completion for lyricslens and lls

for command in lyricslens lls
    # The program takes options only, never a file.
    complete -c $command -f
    complete -c $command -s h -l help -d 'print the help and exit'
    complete -c $command -s V -l version -d 'print the version and exit'
    complete -c $command -l foreground -d 'keep the terminal, instead of letting go of it'
    complete -c $command -l settings -d 'open the preferences window'
    complete -c $command -l toggle -d 'hide the overlay, or bring it back'
    complete -c $command -l song -d 'open the whole song in a window'
    complete -c $command -l position -d 'drag the overlay somewhere else, then press again'
    complete -c $command -l sync -d "the line being sung starts now: fixes this song's timing"
    complete -c $command -l quit -d 'close the overlay that is running'
    complete -c $command -l paths -d 'print where the settings, the cache and the log live'
    complete -c $command -l upgrade -d 'install the newest release over this one'
    complete -c $command -l uninstall -d 'remove the program from ~/.local'
end
