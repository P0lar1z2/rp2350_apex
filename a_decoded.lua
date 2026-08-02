-- Faithfully deobfuscated from a.lua.
-- This file preserves the original behavior; no logic bugs are fixed here.

LMD = 3.0
-- LMD normally corresponds to the in-game mouse sensitivity.

Frequency = 5

XF_KaiGuan = 888
-- 888: Caps Lock; 666: Num Lock; other values are handled as mouse buttons.

Range = (8 // LMD) + 1
Frequency = 5
Decline_range = 6 * LMD
time = GetDate()

ClearLog()
OutputLogMessage("///小枫免费键鼠罗技宏，免费分享请勿倒卖！///\n")
OutputLogMessage("脚本实时更新，目前赛季23赛季,当前时间:%s\n", time)
OutputLogMessage("///免费体验，倒卖可耻///\n")
OutputLogMessage("///B站搜索小枫QAQ，主页加群即可领取，永久免费更新///\n")
OutputLogMessage("///此宏为免费体验版，效果功能与进阶宏有差异///\n")
OutputLogMessage("///需要进阶版本咨询管理员销售客服qq3696569154即可///\n")
OutputLogMessage("///更多优质资源，认准小枫QAQ///\n")

EnablePrimaryMouseButtonEvents(true)

function OnEvent(event, arg)
    time = 0

    if XF_KaiGuan == 888 then
        if IsKeyLockOn("capslock") then
            switch = true
            ClearLog()
            OutputLogMessage("///宏已开启///\n")
            OutputLogMessage("///小枫免费键鼠罗技宏，免费分享请勿倒卖！///\n")
        else
            switch = false
            ClearLog()
            OutputLogMessage("///宏未开启,开关键为CAPSLOCK大小写///\n")
            OutputLogMessage("///小枫免费键鼠罗技宏，免费分享请勿倒卖！///\n")
            OutputLogMessage("///免费体验，倒卖可耻///\n")
            OutputLogMessage("///B站搜索小枫QAQ，主页加群即可领取，永久免费更新///\n")
            OutputLogMessage("///此宏为免费体验版，效果功能与进阶宏有差异///\n")
            OutputLogMessage("///需要进阶版本咨询管理员销售客服qq3696569154即可///\n")
            OutputLogMessage("///更多优质资源，认准小枫QAQ///\n")
        end
    else
        if event == "MOUSE_BUTTON_PRESSED" and arg == XF_KaiGuan then
            switch = not switch
        end
    end

    if XF_KaiGuan == 666 then
        if IsKeyLockOn("numlock") then
            switch = true
            ClearLog()
            OutputLogMessage("///宏已开启///\n")
            OutputLogMessage("///小枫免费键鼠罗技宏，免费分享请勿倒卖！///\n")
        else
            switch = false
            ClearLog()
            OutputLogMessage("///宏未开启,开关键为numlock小键盘锁///\n")
            OutputLogMessage("///小枫免费键鼠罗技宏，免费分享请勿倒卖！///\n")
            OutputLogMessage("///免费体验，倒卖可耻///\n")
            OutputLogMessage("///B站搜索小枫QAQ，主页加群即可领取，永久免费更新///\n")
            OutputLogMessage("///此宏为免费体验版，效果功能与进阶宏有差异///\n")
            OutputLogMessage("///需要进阶版本咨询管理员销售客服qq3696569154即可///\n")
            OutputLogMessage("///更多优质资源，认准小枫QAQ///\n")
        end
    else
        if event == "MOUSE_BUTTON_PRESSED" and arg == XF_KaiGuan then
            switch = not switch
        end
    end

    if switch == true then
        repeat
            MoveMouseRelative(Range, Range)
            Sleep(Frequency)
            time = time + Frequency

            MoveMouseRelative(-Range, -Range)
            Sleep(Frequency)
            time = time + Frequency

            if time >= Decline_range then
                MoveMouseRelative(0, 1)
                time = 0
            end
        until not IsMouseButtonPressed(1)
    end
end
