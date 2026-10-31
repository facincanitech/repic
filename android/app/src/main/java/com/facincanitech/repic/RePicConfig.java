package com.facincanitech.repic;

import android.content.Context;
import android.content.SharedPreferences;

/** Configurações, com os mesmos limites do RePic no Windows. */
final class RePicConfig {

    static final int MIN_SECONDS = 15;
    static final int MAX_SECONDS = 2 * 60 * 60;
    private static final int MIN_LIMIT_MB = 10;
    private static final int MAX_LIMIT_MB = 1_000_000;

    /** Bits por pixel a cada quadro: Leve, Normal, Alta e Máxima. */
    private static final float[] QUALITY_BPP = {0.05f, 0.09f, 0.14f, 0.22f};

    int seconds = 30;
    /** 0 = nativo da tela. */
    int fps = 0;
    /** Menor lado do vídeo: 1080, 720… 0 = nativo da tela. */
    int resolution = 0;
    /** 0 Leve, 1 Normal, 2 Alta, 3 Máxima. */
    int quality = 2;
    boolean audio = true;
    boolean mic = false;
    /** 0 = sem limite. */
    int limitMb = 0;

    static RePicConfig load(Context context) {
        SharedPreferences p = prefs(context);
        RePicConfig c = new RePicConfig();
        c.seconds = p.getInt("seconds", c.seconds);
        c.fps = p.getInt("fps", c.fps);
        c.resolution = p.getInt("resolution", c.resolution);
        c.quality = p.getInt("quality", c.quality);
        c.audio = p.getBoolean("audio", c.audio);
        c.mic = p.getBoolean("mic", c.mic);
        c.limitMb = p.getInt("limitMb", c.limitMb);
        c.validate();
        return c;
    }

    void save(Context context) {
        validate();
        prefs(context)
            .edit()
            .putInt("seconds", seconds)
            .putInt("fps", fps)
            .putInt("resolution", resolution)
            .putInt("quality", quality)
            .putBoolean("audio", audio)
            .putBoolean("mic", mic)
            .putInt("limitMb", limitMb)
            .apply();
    }

    /** Conserta valores inválidos: a interface vem do site e pode mudar. */
    void validate() {
        seconds = clamp(seconds, MIN_SECONDS, MAX_SECONDS);
        quality = clamp(quality, 0, QUALITY_BPP.length - 1);
        fps = clamp(fps, 0, 240);
        resolution = clamp(resolution, 0, 4320);
        limitMb = limitMb <= 0 ? 0 : clamp(limitMb, MIN_LIMIT_MB, MAX_LIMIT_MB);
    }

    int bitrate(int width, int height, int frameRate) {
        long bits = (long) (width * (long) height * frameRate * QUALITY_BPP[quality]);
        return (int) Math.max(500_000L, Math.min(bits, 120_000_000L));
    }

    /** Muda o vídeo que está sendo gravado? Aí a captura precisa recomeçar. */
    boolean sameVideo(RePicConfig other) {
        return fps == other.fps && resolution == other.resolution && quality == other.quality;
    }

    private static int clamp(int value, int min, int max) {
        return Math.max(min, Math.min(max, value));
    }

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences("repic", Context.MODE_PRIVATE);
    }
}
