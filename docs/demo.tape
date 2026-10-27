# Render: vhs docs/demo.tape  (needs vhs, ttyd, ffmpeg; foxmayn-cowork on PATH and configured)
Output docs/demo.gif

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 12
Set TypingSpeed 40ms

# Throwaway directory with synthetic files only
Hide
Type "D=$(mktemp -d) && cd $D && printf 'alpha\n' > notes.txt && printf 'beta\n' > draft.md && printf 'x\n' > old_1.log && printf 'y\n' > old_2.log && clear"
Enter
Type "foxmayn-cowork --dir $D"
Enter
Show

Sleep 2s
Type "List the files here"
Enter
Sleep 5s
Type "Delete all .log files"
Enter
Sleep 4s
Type "y"
Sleep 2s
Ctrl+C
Hide
Type "rm -rf $D"
Enter
