import prisma from '@prisma/client';

export type User = { id: string; handle: string; name: string; publicKey: string };
export type RoomView = { id: string; name: string; members: { id: string; handle: string; name: string }[] };

export class DuplicateHandle extends Error {}

export interface Store {
  userByKey(publicKey: string): Promise<User | null>;
  userById(id: string): Promise<User | null>;
  createUser(u: Omit<User, 'id'>): Promise<User>;
  renameUser(id: string, name: string): Promise<User>;
  rooms(): Promise<RoomView[]>;
  room(id: string): Promise<RoomView | null>;
  // Server time the room was created, in epoch milliseconds.
  roomCreated(id: string): Promise<number | null>;
  createRoom(name: string, ownerId: string): Promise<RoomView>;
  join(roomId: string, userId: string): Promise<void>;
  leave(roomId: string, userId: string): Promise<void>;
  isMember(roomId: string, userId: string): Promise<boolean>;
  clientOwners(clientIds: number[]): Promise<Map<number, string>>;
  updates(roomId: string): Promise<Uint8Array[]>;
  // Stores the update and binds the new client ids in one transaction.
  appendUpdate(roomId: string, data: Uint8Array, userId: string, clientIds: number[]): Promise<void>;
}

const roomSelect = { id: true, name: true, members: { select: { user: { select: { id: true, handle: true, name: true } } } } };
type RoomRow = { id: string; name: string; members: { user: RoomView['members'][number] }[] };
const toView = (r: RoomRow): RoomView => ({ id: r.id, name: r.name, members: r.members.map((m) => m.user) });

export class PrismaStore implements Store {
  db = new prisma.PrismaClient();

  userByKey(publicKey: string) {
    return this.db.user.findUnique({ where: { publicKey } });
  }

  userById(id: string) {
    return this.db.user.findUnique({ where: { id } });
  }

  async createUser(u: Omit<User, 'id'>) {
    try {
      return await this.db.user.create({ data: u });
    } catch (e) {
      if (e instanceof prisma.Prisma.PrismaClientKnownRequestError && e.code === 'P2002') throw new DuplicateHandle();
      throw e;
    }
  }

  renameUser(id: string, name: string) {
    return this.db.user.update({ where: { id }, data: { name } });
  }

  async rooms() {
    const rows = await this.db.room.findMany({ select: roomSelect, orderBy: { createdAt: 'asc' } });
    return rows.map(toView);
  }

  async room(id: string) {
    const row = await this.db.room.findUnique({ where: { id }, select: roomSelect });
    return row && toView(row);
  }

  async roomCreated(id: string) {
    const row = await this.db.room.findUnique({ where: { id }, select: { createdAt: true } });
    return row && row.createdAt.getTime();
  }

  async createRoom(name: string, ownerId: string) {
    const row = await this.db.room.create({ data: { name, members: { create: { userId: ownerId } } }, select: roomSelect });
    return toView(row);
  }

  async join(roomId: string, userId: string) {
    await this.db.member.upsert({ where: { roomId_userId: { roomId, userId } }, create: { roomId, userId }, update: {} });
  }

  async leave(roomId: string, userId: string) {
    await this.db.member.deleteMany({ where: { roomId, userId } });
  }

  async isMember(roomId: string, userId: string) {
    return (await this.db.member.count({ where: { roomId, userId } })) > 0;
  }

  async clientOwners(clientIds: number[]) {
    const rows = await this.db.client.findMany({ where: { id: { in: clientIds.map(BigInt) } } });
    return new Map(rows.map((r) => [Number(r.id), r.userId]));
  }

  async updates(roomId: string) {
    const rows = await this.db.roomUpdate.findMany({ where: { roomId }, orderBy: { seq: 'asc' } });
    return rows.map((r) => new Uint8Array(r.data));
  }

  async appendUpdate(roomId: string, data: Uint8Array, userId: string, clientIds: number[]) {
    await this.db.$transaction([
      this.db.client.createMany({ data: clientIds.map((id) => ({ id: BigInt(id), userId })), skipDuplicates: true }),
      this.db.roomUpdate.create({ data: { roomId, data: Buffer.from(data) } }),
    ]);
  }
}
