# Generate the "08 Classroom" speech bubbles: a 3D emoji icon, the English phrase and its
# Chinese (simplified) translation, in a glossy bubble colored by kind of phrase.
#   powershell -ExecutionPolicy Bypass -File tools\effects-art\classroom.ps1
# Then render with effects-art. Each line below: English | Chinese | Fluent emoji | theme.
$phrases = @'
Hello!|你好！|Waving hand|hi
Good morning!|早上好！|Sunrise|hi
Good afternoon!|下午好！|Sun|hi
Good evening!|晚上好！|Crescent moon|hi
How are you?|你好吗？|Grinning face|hi
Nice to see you!|很高兴见到你！|Smiling face with hearts|hi
Bye-bye!|再见！|Waving hand|hi
See you next time!|下次见！|Waving hand|hi
Can you hear me?|你能听到我吗？|Ear|tech
Can you see me?|你能看到我吗？|Eyes|tech
I can't hear you.|我听不到你。|Muted speaker|tech
Turn on your microphone.|请打开麦克风。|Microphone|tech
Turn on your camera.|请打开摄像头。|Video camera|tech
Mute your microphone.|请关闭麦克风。|Muted speaker|tech
Come closer to the camera.|请靠近摄像头。|Video camera|tech
Look at the screen.|请看屏幕。|Laptop|tech
Use the pen.|请用画笔。|Writing hand|tech
Write on the board.|请在白板上写。|Pencil|tech
Click here.|请点这里。|Backhand index pointing down|tech
Listen!|请听！|Ear|do
Look!|请看！|Eyes|do
Repeat after me.|跟我读。|Speaking head|do
Say it again, please.|请再说一遍。|Repeat button|do
Louder, please!|请大声一点！|Loudspeaker|do
Slowly, please.|请慢一点。|Stopwatch|do
Raise your hand.|请举手。|Raised hand|do
Answer the question.|请回答问题。|Person raising hand|do
Your turn!|轮到你了！|Index pointing up|do
My turn.|轮到我了。|Teacher|do
Read together.|我们一起读。|Open book|do
Open your book.|请打开书。|Open book|do
Close your book.|请合上书。|Books|do
Write it down.|请写下来。|Memo|do
Draw a picture.|请画一幅画。|Artist palette|do
Point to the picture.|请指着图片。|Backhand index pointing right|do
Show me!|给我看看！|Eyes|do
Clap your hands!|拍拍手！|Clapping hands|do
Touch your nose.|摸摸你的鼻子。|Nose|do
Stand up, please.|请站起来。|Person standing|do
Sit down, please.|请坐下。|Chair|do
Think about it.|想一想。|Thinking face|do
Quiet, please.|请安静。|Shushing face|do
Wait a moment.|请稍等。|Hourglass not done|do
Hurry up!|快一点！|Alarm clock|do
Any questions?|有问题吗？|Red question mark|ask
Do you understand?|你明白了吗？|Face with monocle|ask
Are you ready?|准备好了吗？|Chequered flag|ask
What's this?|这是什么？|Red question mark|ask
What color is it?|这是什么颜色？|Artist palette|ask
How many?|有多少？|Thinking face|ask
How old are you?|你几岁了？|Birthday cake|ask
What's your name?|你叫什么名字？|Speech balloon|ask
Correct!|正确！|Check mark button|good
Yes!|是的！|Thumbs up|good
Good job!|做得好！|Thumbs up|good
Well done!|干得好！|Clapping hands|good
Great idea!|好主意！|Light bulb|good
Keep going!|继续加油！|Flexed biceps|good
Almost!|差一点！|Pinching hand|good
Excellent!|非常好！|Glowing star|praise
Perfect!|完美！|Hundred points|praise
Amazing!|太厉害了！|Star-struck|praise
Super star!|超级明星！|Star|praise
You are the best!|你是最棒的！|Trophy|praise
Wow!|哇！|Exploding head|praise
I'm proud of you!|我为你骄傲！|Red heart|praise
Try again!|再试一次！|Repeat button|no
Not quite.|不太对。|Thinking face|no
No.|不对。|Cross mark|no
Don't give up!|不要放弃！|Flexed biceps|no
Let's play a game!|我们来玩游戏吧！|Video game|fun
Let's sing a song!|我们来唱歌吧！|Musical notes|fun
Let's start!|我们开始吧！|Rocket|fun
Let's review.|我们来复习。|Books|fun
One more time!|再来一次！|Repeat button|fun
Time's up!|时间到！|Alarm clock|fun
Break time!|休息时间！|Hugging face|fun
Homework time!|作业时间！|House|fun
Finished!|完成了！|Chequered flag|fun
Happy birthday!|生日快乐！|Birthday cake|fun
Good luck!|祝你好运！|Four leaf clover|fun
Thank you!|谢谢！|Folded hands|fun
You're welcome.|不客气。|Smiling face with halo|fun
Sorry!|对不起！|Folded hands|fun
'@
$themes = @{
    hi     = @('#eaf5ff', '#b9dcff', '#2f7fd8', '#0d3b73')
    tech   = @('#e4fbf8', '#aaeae1', '#159c8c', '#064a42')
    do     = @('#f3edff', '#d6c4ff', '#7b52d6', '#331a73')
    ask    = @('#fff3e3', '#ffd09a', '#e5801c', '#733500')
    good   = @('#e8fbea', '#b2edbd', '#2e9e4f', '#0d4a21')
    praise = @('#fff9da', '#ffdf6e', '#d99a00', '#6e4100')
    no     = @('#ffeded', '#ffc0c0', '#d93636', '#731010')
    fun    = @('#ffedf5', '#ffc2db', '#e0457f', '#701038')
}
$dir = Join-Path $PSScriptRoot '..\..\assets\effects\08 Classroom'
if (Test-Path $dir) { Get-ChildItem $dir -Filter *.svg | Remove-Item }
else { $null = New-Item -ItemType Directory $dir }
function Esc($s) { $s.Replace('&', '&amp;').Replace('<', '&lt;').Replace("'", '&#39;') }
$n = 0
foreach ($line in ($phrases -split "`r?`n" | Where-Object { $_.Trim() })) {
    $en, $zh, $icon, $th = $line.Split('|')
    $c = $themes[$th]
    # Fit the text into the 540-wide text area.
    # Approximate Segoe UI Black advance widths (em) per character.
    function Em($str) {
        $em = 0.0
        foreach ($ch in $str.ToCharArray()) {
            $s = [string]$ch
            $em += if ($s -eq ' ') { 0.26 } elseif ('!.,?'''.Contains($s)) { 0.32 } elseif ($s -cmatch '^[A-Z]$') { 0.7 } elseif ('mw'.Contains($s)) { 0.85 } elseif ('ijlt'.Contains($s)) { 0.33 } else { 0.58 }
        }
        $em
    }
    $lines = @($en)
    if ((Em $en) -gt 11 -and $en.Contains(' ')) {
        # Two lines, split at the space nearest the middle.
        $best = $null
        for ($k = 1; $k -lt $en.Length; $k++) {
            if ($en[$k] -ne ' ') { continue }
            $d = [math]::Abs((Em $en.Substring(0, $k)) - (Em $en.Substring($k + 1)))
            if ($null -eq $best -or $d -lt $best[0]) { $best = @($d, $k) }
        }
        $lines = @($en.Substring(0, $best[1]), $en.Substring($best[1] + 1))
    }
    $wide = ($lines | ForEach-Object { Em $_ } | Measure-Object -Maximum).Maximum
    $enSize = [math]::Round([math]::Min($(if ($lines.Count -gt 1) { 50 } else { 68 }), 490 / $wide), 1)
    $zhSize = [math]::Round([math]::Min($(if ($lines.Count -gt 1) { 44 } else { 52 }), 490 / ($zh.Length * 1.02)), 1)
    $file = ($en -replace "[^A-Za-z0-9 ']", '').Trim()
    $svg = @"
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 320">
  <defs>
    <linearGradient id="f" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="$($c[0])"/><stop offset="1" stop-color="$($c[1])"/></linearGradient>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.85"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>
    <filter id="s" x="-10%" y="-10%" width="120%" height="130%"><feDropShadow dx="0" dy="8" stdDeviation="9" flood-color="#000" flood-opacity="0.35"/></filter>
  </defs>
  <g filter="url(#s)">
    <path d="M80 24 H720 A56 56 0 0 1 776 80 V190 A56 56 0 0 1 720 246 H190 L96 302 L118 246 H80 A56 56 0 0 1 24 190 V80 A56 56 0 0 1 80 24 Z" fill="url(#f)" stroke="$($c[2])" stroke-width="7" stroke-linejoin="round"/>
  </g>
  <path d="M86 36 H714 A44 44 0 0 1 758 80 V96 Q420 128 42 96 V80 A44 44 0 0 1 86 36 Z" fill="url(#g)"/>
  <circle cx="130" cy="135" r="82" fill="#ffffff" fill-opacity="0.75" stroke="$($c[2])" stroke-opacity="0.35" stroke-width="4"/>
  <image href="emoji:fluent:$icon" x="62" y="67" width="136" height="136"/>
$(if ($lines.Count -gt 1) { "  <text x=`"495`" y=`"94`" text-anchor=`"middle`" font-family=`"Segoe UI`" font-weight=`"900`" font-size=`"$enSize`" fill=`"$($c[3])`">$(Esc $lines[0])</text>`n  <text x=`"495`" y=`"150`" text-anchor=`"middle`" font-family=`"Segoe UI`" font-weight=`"900`" font-size=`"$enSize`" fill=`"$($c[3])`">$(Esc $lines[1])</text>" } else { "  <text x=`"495`" y=`"128`" text-anchor=`"middle`" font-family=`"Segoe UI`" font-weight=`"900`" font-size=`"$enSize`" fill=`"$($c[3])`">$(Esc $en)</text>" })
  <text x="495" y="$(if ($lines.Count -gt 1) { 210 } else { 208 })" text-anchor="middle" font-family="Microsoft YaHei" font-weight="700" font-size="$zhSize" fill="$($c[2])">$(Esc $zh)</text>
</svg>
"@
    [IO.File]::WriteAllText((Join-Path $dir "$file.svg"), $svg, (New-Object Text.UTF8Encoding($false)))
    $n++
}
"$n bubbles"
