package com.facincanitech.repic;

import android.media.MediaCodec;
import java.io.File;
import java.io.IOException;
import java.io.RandomAccessFile;
import java.nio.ByteBuffer;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;

/**
 * Guarda em disco as amostras já codificadas, em arquivos que vão sendo apagados.
 * Na memória fica só o índice: tempos longos não enchem a RAM.
 */
final class SampleRing {

    static final class Sample {
        final long ptsUs;
        final long pos;
        final int size;
        final int flags;
        final int file;

        Sample(long ptsUs, long pos, int size, int flags, int file) {
            this.ptsUs = ptsUs;
            this.pos = pos;
            this.size = size;
            this.flags = flags;
            this.file = file;
        }

        boolean isKey() {
            return (flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0;
        }
    }

    private static final long BYTES_PER_FILE = 32L * 1024 * 1024;

    private final File dir;
    private final ArrayDeque<Sample> samples = new ArrayDeque<>();
    private RandomAccessFile writer;
    private int currentFile = -1;
    private int oldestFile = 0;
    private long filePos = 0;
    private long totalBytes = 0;
    /** Pedidos de não apagar nada (clipe salvando, gravação rodando). */
    private int holds = 0;

    SampleRing(File dir) {
        this.dir = dir;
        dir.mkdirs();
        deleteFiles();
    }

    private File fileOf(int id) {
        return new File(dir, id + ".bin");
    }

    private void deleteFiles() {
        File[] files = dir.listFiles();
        if (files != null) {
            for (File f : files) {
                f.delete();
            }
        }
    }

    synchronized void append(ByteBuffer data, long ptsUs, int flags) throws IOException {
        int size = data.remaining();
        if (writer == null || filePos >= BYTES_PER_FILE) {
            if (writer != null) {
                writer.close();
            }
            currentFile++;
            writer = new RandomAccessFile(fileOf(currentFile), "rw");
            filePos = 0;
        }
        long pos = filePos;
        while (data.hasRemaining()) {
            filePos += writer.getChannel().write(data, filePos);
        }
        samples.addLast(new Sample(ptsUs, pos, size, flags, currentFile));
        totalBytes += size;
    }

    /** Total já escrito, pro medidor de tamanho da gravação. */
    synchronized long totalBytes() {
        return totalBytes;
    }

    synchronized void hold() {
        holds++;
    }

    synchronized void release() {
        holds = Math.max(0, holds - 1);
    }

    /** Esquece o que é mais velho que `minPtsUs` e apaga os arquivos sem uso. */
    synchronized void prune(long minPtsUs) {
        if (holds > 0) {
            return;
        }
        while (!samples.isEmpty() && samples.peekFirst().ptsUs < minPtsUs) {
            samples.pollFirst();
        }
        int firstInUse = samples.isEmpty() ? currentFile : samples.peekFirst().file;
        while (oldestFile < firstInUse) {
            fileOf(oldestFile).delete();
            oldestFile++;
        }
    }

    /**
     * Amostras entre os dois tempos. Com `fromKey`, recua até o quadro-chave anterior:
     * vídeo só pode começar num quadro completo.
     */
    synchronized List<Sample> snapshot(long fromUs, long toUs, boolean fromKey) {
        List<Sample> out = new ArrayList<>();
        Sample lastKeyBefore = null;
        List<Sample> sinceKey = new ArrayList<>();
        for (Sample s : samples) {
            if (s.ptsUs > toUs) {
                break;
            }
            if (s.ptsUs < fromUs) {
                if (fromKey) {
                    if (s.isKey()) {
                        lastKeyBefore = s;
                        sinceKey.clear();
                    }
                    if (lastKeyBefore != null) {
                        sinceKey.add(s);
                    }
                }
                continue;
            }
            out.add(s);
        }
        if (fromKey && !sinceKey.isEmpty()) {
            sinceKey.addAll(out);
            return sinceKey;
        }
        return out;
    }

    /** Lê uma amostra; o arquivo pode estar aberto pra escrita ao mesmo tempo. */
    void read(Sample s, ByteBuffer into, Readers readers) throws IOException {
        into.clear();
        into.limit(s.size);
        RandomAccessFile file = readers.open(s.file, fileOf(s.file));
        long pos = s.pos;
        while (into.hasRemaining()) {
            int n = file.getChannel().read(into, pos);
            if (n < 0) {
                throw new IOException("fim inesperado do arquivo");
            }
            pos += n;
        }
        into.flip();
    }

    /** Mantém um arquivo aberto por vez durante a leitura de um clipe. */
    static final class Readers implements AutoCloseable {
        private int openId = -1;
        private RandomAccessFile open;

        RandomAccessFile open(int id, File file) throws IOException {
            if (id != openId) {
                close();
                open = new RandomAccessFile(file, "r");
                openId = id;
            }
            return open;
        }

        @Override
        public void close() throws IOException {
            if (open != null) {
                open.close();
                open = null;
                openId = -1;
            }
        }
    }

    /** Apaga tudo (a tela girou e o vídeo recomeçou, ou a captura parou). */
    synchronized void clear() {
        samples.clear();
        if (writer != null) {
            try {
                writer.close();
            } catch (IOException ignored) {
                // Já vai ser apagado.
            }
            writer = null;
        }
        deleteFiles();
        oldestFile = currentFile + 1;
        filePos = 0;
    }
}
